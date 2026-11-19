//! `OverlayConfig` and its `overlay.toml` load/default path (Phase 6) — a
//! separate file from `agent.toml` since the overlay is an optional add-on
//! nobody has to configure just to run the agent/tray/UI, not another
//! field on `AgentConfig`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverlayCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayConfig {
    /// Which corner of the monitor to anchor to.
    pub corner: OverlayCorner,
    /// Index into the OS's monitor list (0 is whichever it reports first,
    /// not necessarily the Windows-designated "primary" one). Out of range
    /// (a monitor unplugged since this was set) falls back to 0.
    pub monitor_index: usize,
    /// Distance from the two edges of `corner`, in logical (DPI-independent) pixels.
    pub margin_px: f32,
    /// 0.0 (invisible) to 1.0 (opaque). `f64`, not `f32`, because TOML
    /// floats are 64-bit: an `f32` 0.85 was written to the generated
    /// `overlay.toml` as `0.8500000238418579`.
    pub opacity: f64,
    /// How often to poll the agent's `live` command and repaint.
    pub refresh_interval_ms: u64,
    /// Global hotkey that toggles the overlay on/off, in `global_hotkey`'s
    /// accelerator string syntax (e.g. `"Ctrl+Alt+O"`). An unparsable value
    /// (hand-edited) disables the hotkey rather than failing to start —
    /// the tray's menu item still toggles it.
    pub hotkey: String,
    /// Where the overlay was last dragged to in edit mode ("Move Overlay"
    /// in the tray): its top-left corner in logical px, on the whole
    /// desktop rather than one monitor. When set, it replaces `corner`,
    /// `monitor_index` and `margin_px`; delete the line to go back to them.
    /// Ignored, with the corner used instead, if it's no longer on any
    /// monitor (e.g. one that has since been unplugged).
    pub position: Option<[f64; 2]>,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            corner: OverlayCorner::TopRight,
            monitor_index: 0,
            margin_px: 12.0,
            opacity: 0.85,
            refresh_interval_ms: 500,
            hotkey: "Ctrl+Alt+O".to_string(),
            position: None,
        }
    }
}

/// The commands the tray sends the overlay, one per line on its stdin: the
/// tray launches the overlay and keeps the pipe, so this needs no port or
/// file. Here so both ends spell them the same.
pub const OVERLAY_EDIT_ON: &str = "edit on";
pub const OVERLAY_EDIT_OFF: &str = "edit off";

impl OverlayConfig {
    pub fn load_or_default(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&text)?)
        } else {
            let cfg = Self::default();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, toml::to_string(&cfg)?)?;
            Ok(cfg)
        }
    }

    pub fn default_config_path() -> PathBuf {
        crate::data_dir().join("overlay.toml")
    }

    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Not `to_string_pretty`: it spreads `position = [x, y]` over four lines.
        std::fs::write(path, toml::to_string(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkey_parses_as_a_global_hotkey_accelerator() {
        // Not a round-trip through the `global_hotkey` crate (agent_config
        // doesn't depend on it — only crates/overlay does), just a sanity
        // check on the string shape it expects: `+`-joined tokens, modifiers
        // before the final key.
        let cfg = OverlayConfig::default();
        let parts: Vec<&str> = cfg.hotkey.split('+').collect();
        assert!(parts.len() >= 2, "{:?}", cfg.hotkey);
        assert!(["Ctrl", "Alt", "Shift", "Super"].contains(parts.first().unwrap()));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: OverlayConfig = toml::from_str("opacity = 0.5").unwrap();
        assert_eq!(cfg.opacity, 0.5);
        assert_eq!(cfg.corner, OverlayCorner::TopRight);
        assert_eq!(
            cfg.refresh_interval_ms,
            OverlayConfig::default().refresh_interval_ms
        );
    }

    #[test]
    fn save_then_load_or_default_roundtrips() {
        let path = std::env::temp_dir().join(format!(
            "ghostperf-overlay-config-test-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut cfg = OverlayConfig::default();
        cfg.opacity = 0.42;
        cfg.corner = OverlayCorner::BottomLeft;
        cfg.monitor_index = 1;
        cfg.save(&path).unwrap();

        let loaded = OverlayConfig::load_or_default(&path).unwrap();
        assert_eq!(loaded.opacity, 0.42);
        assert_eq!(loaded.corner, OverlayCorner::BottomLeft);
        assert_eq!(loaded.monitor_index, 1);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_saved_position_roundtrips_and_no_position_is_left_out() {
        let text = toml::to_string(&OverlayConfig::default()).unwrap();
        assert!(!text.contains("position"), "{text}");
        assert_eq!(
            toml::from_str::<OverlayConfig>(&text).unwrap().position,
            None
        );

        let cfg = OverlayConfig {
            position: Some([1520.0, 36.0]),
            ..OverlayConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("position = [1520.0, 36.0]"), "{text}");
        assert_eq!(
            toml::from_str::<OverlayConfig>(&text).unwrap().position,
            Some([1520.0, 36.0])
        );
    }
}
