//! Task-based e-ink display modes (Reading / Browsing / Writing / Video).
//!
//! A **mode** is a named bundle of pipeline + refresh settings that trades
//! update speed against image quality and stability. Modes are **overlays on
//! the user's base config**: only the fields a mode names are changed, so
//! per-user settings (e.g. `invert`) survive every switch.
//!
//! This lives in the binary — not `papercast-core` — on purpose:
//! `papercast-core` owns pixel/pipeline concepts only, while fps / tile size /
//! refresh policy are mirror-runtime concerns. Core stays free of them.
//!
//! [`ModeState`] is the single source of truth: base settings + active mode
//! name + custom mode definitions → *effective* settings. Config hot-reload
//! updates the base; `ctl mode` changes the active name; both read back the
//! same effective settings, so neither clobbers the other.

use std::collections::BTreeMap;

use papercast_core::dither::DitherMode;
use papercast_core::scale::FitMode;
use papercast_core::EinkConfig;
use serde::Deserialize;

/// The effective runtime settings the serve loop and pipeline consume.
/// `eink` drives the pipeline thread; the rest drive the serve loop.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeSettings {
    pub eink: EinkConfig,
    pub fps: u32,
    pub tile_size: u32,
    pub full_refresh_secs: u64,
    pub full_refresh_updates: u64,
}

/// Per-field overrides a mode lays over the base config. Every field is
/// optional so we can tell "the mode sets this" from "inherit the base".
/// Deserialized directly from a `[modes.<name>]` TOML table (same keys as
/// `[eink]`, minus `target-size`/output-size which is fixed at startup, plus
/// the mirror-side keys).
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ModeOverlay {
    // Pipeline (eink) overrides.
    pub contrast: Option<f32>,
    pub gamma: Option<f32>,
    pub black_point: Option<u8>,
    pub white_point: Option<u8>,
    pub invert: Option<bool>,
    pub sharpen: Option<f32>,
    pub sharpen_radius: Option<u32>,
    pub dither: Option<DitherMode>,
    pub levels: Option<u8>,
    pub fit: Option<FitMode>,
    // Mirror-side overrides.
    pub fps: Option<u32>,
    pub tile_size: Option<u32>,
    pub full_refresh_secs: Option<u64>,
    pub full_refresh_updates: Option<u64>,
}

impl ModeOverlay {
    /// Apply this overlay onto `base`, producing effective settings. Only the
    /// `Some` fields override; everything else is inherited (notably
    /// `target_size`, which modes never touch).
    fn apply(&self, base: &ModeSettings) -> ModeSettings {
        let mut out = base.clone();
        let e = &mut out.eink;
        if let Some(v) = self.contrast {
            e.contrast = v;
        }
        if let Some(v) = self.gamma {
            e.gamma = v;
        }
        if let Some(v) = self.black_point {
            e.black_point = v;
        }
        if let Some(v) = self.white_point {
            e.white_point = v;
        }
        if let Some(v) = self.invert {
            e.invert = v;
        }
        if let Some(v) = self.sharpen {
            e.sharpen = v;
        }
        if let Some(v) = self.sharpen_radius {
            e.sharpen_radius = v;
        }
        if let Some(v) = self.dither {
            e.dither = v;
        }
        if let Some(v) = self.levels {
            e.levels = v;
        }
        if let Some(v) = self.fit {
            e.fit = v;
        }
        if let Some(v) = self.fps {
            out.fps = v;
        }
        if let Some(v) = self.tile_size {
            out.tile_size = v;
        }
        if let Some(v) = self.full_refresh_secs {
            out.full_refresh_secs = v;
        }
        if let Some(v) = self.full_refresh_updates {
            out.full_refresh_updates = v;
        }
        out
    }

    /// Merge `other` on top of `self` (other's `Some` fields win). Used to let
    /// a config `[modes.<name>]` override a built-in of the same name.
    fn merged_with(&self, other: &ModeOverlay) -> ModeOverlay {
        ModeOverlay {
            contrast: other.contrast.or(self.contrast),
            gamma: other.gamma.or(self.gamma),
            black_point: other.black_point.or(self.black_point),
            white_point: other.white_point.or(self.white_point),
            invert: other.invert.or(self.invert),
            sharpen: other.sharpen.or(self.sharpen),
            sharpen_radius: other.sharpen_radius.or(self.sharpen_radius),
            dither: other.dither.or(self.dither),
            levels: other.levels.or(self.levels),
            fit: other.fit.or(self.fit),
            fps: other.fps.or(self.fps),
            tile_size: other.tile_size.or(self.tile_size),
            full_refresh_secs: other.full_refresh_secs.or(self.full_refresh_secs),
            full_refresh_updates: other.full_refresh_updates.or(self.full_refresh_updates),
        }
    }
}

/// Names of the built-in modes, for error messages and `max_fps`.
pub const BUILTIN_NAMES: [&str; 4] = ["reading", "browsing", "writing", "video"];

/// The built-in overlay for `name`, or `None` if `name` isn't a built-in.
///
/// Intents (see the "Display modes" table in `README.md`):
/// - `reading`: max quality; every change (page turn) earns a clean full redraw.
/// - `browsing`: balanced default, ≈ today's behavior.
/// - `writing`: min latency; few levels = crisp text and cheap updates.
/// - `video`: motion; no sharpen halos, never interrupt with a full redraw.
fn builtin(name: &str) -> Option<ModeOverlay> {
    let base = ModeOverlay {
        dither: Some(DitherMode::Bayer),
        levels: Some(16),
        ..Default::default()
    };
    let o = match name {
        "reading" => ModeOverlay {
            fps: Some(5),
            sharpen: Some(1.0),
            tile_size: Some(64),
            full_refresh_secs: Some(0),
            full_refresh_updates: Some(1),
            ..base
        },
        "browsing" => ModeOverlay {
            fps: Some(15),
            sharpen: Some(1.0),
            tile_size: Some(64),
            full_refresh_secs: Some(60),
            full_refresh_updates: Some(0),
            ..base
        },
        "writing" => ModeOverlay {
            fps: Some(30),
            levels: Some(4),
            sharpen: Some(1.5),
            tile_size: Some(32),
            full_refresh_secs: Some(300),
            full_refresh_updates: Some(0),
            ..base
        },
        "video" => ModeOverlay {
            fps: Some(30),
            sharpen: Some(0.0),
            tile_size: Some(64),
            full_refresh_secs: Some(0),
            full_refresh_updates: Some(0),
            ..base
        },
        _ => return None,
    };
    Some(o)
}

/// Error selecting or resolving a mode. The binary is `anyhow`-based (core is
/// the `thiserror` crate), so this is a plain `std::error::Error`.
#[derive(Debug)]
pub struct UnknownMode {
    pub name: String,
    pub valid: Vec<String>,
}

impl std::fmt::Display for UnknownMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown mode {:?}; valid modes: {}", self.name, self.valid.join(", "))
    }
}

impl std::error::Error for UnknownMode {}

/// Central mode-state manager: the single source of truth for what settings
/// are in effect. Holds the base config, the active mode name (if any), and
/// any custom/overriding mode definitions from the config file.
#[derive(Debug, Clone)]
pub struct ModeState {
    base: ModeSettings,
    active: Option<String>,
    custom: BTreeMap<String, ModeOverlay>,
}

impl ModeState {
    /// Build a manager. `active` (from `--mode`/`[mirror].mode`) must resolve
    /// to a built-in or a custom mode, else this errors.
    pub fn new(
        base: ModeSettings,
        custom: BTreeMap<String, ModeOverlay>,
        active: Option<String>,
    ) -> Result<Self, UnknownMode> {
        let state = Self { base, active: None, custom };
        match active {
            Some(name) => {
                let mut state = state;
                state.set_mode(&name)?;
                Ok(state)
            }
            None => Ok(state),
        }
    }

    /// Every selectable mode name (built-ins + custom), sorted and deduped.
    pub fn valid_names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTIN_NAMES.iter().map(|s| s.to_string()).collect();
        for k in self.custom.keys() {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
        names.sort();
        names
    }

    /// The overlay for `name` = built-in (if any) with the config's
    /// `[modes.<name>]` merged on top. `None` if `name` is neither.
    fn overlay_for(&self, name: &str) -> Option<ModeOverlay> {
        match (builtin(name), self.custom.get(name)) {
            (Some(b), Some(c)) => Some(b.merged_with(c)),
            (Some(b), None) => Some(b),
            (None, Some(c)) => Some(c.clone()),
            (None, None) => None,
        }
    }

    /// The settings currently in effect: base, plus the active mode's overlay.
    pub fn effective(&self) -> ModeSettings {
        match &self.active {
            None => self.base.clone(),
            // Safe: `active` is only ever set to a name that resolved.
            Some(name) => self.overlay_for(name).expect("active mode resolves").apply(&self.base),
        }
    }

    /// The active mode name, or `None` for plain base config.
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Switch the active mode. Errors (listing valid names) if unknown.
    pub fn set_mode(&mut self, name: &str) -> Result<(), UnknownMode> {
        if self.overlay_for(name).is_none() {
            return Err(UnknownMode { name: name.to_string(), valid: self.valid_names() });
        }
        self.active = Some(name.to_string());
        Ok(())
    }

    /// Replace the base `[eink]` config (hot-reload path). The active mode's
    /// overlay is re-applied on the next `effective()`, so an edit to the
    /// config never drops the active mode.
    pub fn set_base_eink(&mut self, eink: EinkConfig) {
        self.base.eink = eink;
    }

    /// The base eink config (what hot-reload compares against for changes).
    pub fn base_eink(&self) -> &EinkConfig {
        &self.base.eink
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ModeSettings {
        ModeSettings {
            eink: EinkConfig { invert: true, contrast: 1.7, ..Default::default() },
            fps: 15,
            tile_size: 64,
            full_refresh_secs: 60,
            full_refresh_updates: 0,
        }
    }

    #[test]
    fn builtin_lookup_matches_table() {
        let st = ModeState::new(base(), BTreeMap::new(), Some("writing".into())).unwrap();
        let e = st.effective();
        assert_eq!(e.fps, 30);
        assert_eq!(e.eink.levels, 4);
        assert_eq!(e.eink.sharpen, 1.5);
        assert_eq!(e.tile_size, 32);
        assert_eq!(e.full_refresh_secs, 300);
        assert_eq!(e.full_refresh_updates, 0);
    }

    #[test]
    fn overlay_preserves_user_base_fields() {
        // The user set invert + contrast in their base config; no mode touches
        // them, so every mode must keep them.
        for name in BUILTIN_NAMES {
            let st = ModeState::new(base(), BTreeMap::new(), Some(name.into())).unwrap();
            let e = st.effective();
            assert!(e.eink.invert, "{name} dropped invert");
            assert_eq!(e.eink.contrast, 1.7, "{name} dropped contrast");
        }
    }

    #[test]
    fn no_active_mode_is_plain_base() {
        let st = ModeState::new(base(), BTreeMap::new(), None).unwrap();
        assert_eq!(st.effective(), base());
        assert_eq!(st.active(), None);
    }

    #[test]
    fn unknown_mode_errors_and_lists_valid_names() {
        let err = ModeState::new(base(), BTreeMap::new(), Some("nope".into())).unwrap_err();
        assert_eq!(err.name, "nope");
        assert_eq!(err.valid, ["browsing", "reading", "video", "writing"]);
    }

    #[test]
    fn custom_mode_overrides_builtin_field() {
        // Config raises reading's fps but leaves the rest of the built-in.
        let mut custom = BTreeMap::new();
        custom.insert("reading".to_string(), ModeOverlay { fps: Some(8), ..Default::default() });
        let st = ModeState::new(base(), custom, Some("reading".into())).unwrap();
        let e = st.effective();
        assert_eq!(e.fps, 8); // overridden
        assert_eq!(e.full_refresh_updates, 1); // built-in preserved
    }

    #[test]
    fn custom_defines_a_new_mode() {
        let mut custom = BTreeMap::new();
        custom.insert(
            "mine".to_string(),
            ModeOverlay { fps: Some(3), levels: Some(2), ..Default::default() },
        );
        let mut st = ModeState::new(base(), custom, None).unwrap();
        assert!(st.valid_names().contains(&"mine".to_string()));
        st.set_mode("mine").unwrap();
        let e = st.effective();
        assert_eq!(e.fps, 3);
        assert_eq!(e.eink.levels, 2);
    }

    #[test]
    fn hot_reload_base_keeps_active_mode() {
        let mut st = ModeState::new(base(), BTreeMap::new(), Some("writing".into())).unwrap();
        // Simulate a config edit changing gamma; mode must stay active.
        st.set_base_eink(EinkConfig { gamma: 0.8, invert: true, ..Default::default() });
        let e = st.effective();
        assert_eq!(st.active(), Some("writing"));
        assert_eq!(e.eink.gamma, 0.8); // new base value flows through
        assert_eq!(e.eink.levels, 4); // writing overlay still applied
    }

}
