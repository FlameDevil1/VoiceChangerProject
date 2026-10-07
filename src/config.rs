//! Settings persisted to `%APPDATA%\VoiceChanger\config.json`.

use crate::dsp::FxSettings;
use crate::hotkeys::HotkeyConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const CONFIG_VERSION: u32 = 1;

/// A remembered device: the stable cpal ID plus its name for display/fallback matching.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatencyMode {
    /// Smallest buffers; may glitch on busy systems (auto-grows on underrun).
    Low,
    #[default]
    Balanced,
    /// Extra headroom for slow or heavily loaded machines.
    Safe,
}

impl LatencyMode {
    /// Safety margin added on top of the device block sizes.
    pub fn margin_seconds(self) -> f64 {
        match self {
            LatencyMode::Low => 0.002,
            LatencyMode::Balanced => 0.006,
            LatencyMode::Safe => 0.020,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LatencyMode::Low => "Low",
            LatencyMode::Balanced => "Balanced",
            LatencyMode::Safe => "Safe",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiMode {
    /// Presets, devices and the essentials.
    #[default]
    Simple,
    /// Every control, effect order and preset management.
    Advanced,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemePref {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// `None` = follow the Windows default device.
    pub input: Option<DeviceRef>,
    /// Virtual cable playback device ("CABLE Input"). `None` = not routed.
    pub cable: Option<DeviceRef>,
    /// Headphones for self-monitoring. `None` = Windows default output.
    pub monitor: Option<DeviceRef>,
    pub monitor_enabled: bool,
    /// `None` = average all channels.
    pub input_channel: Option<usize>,
    pub latency: LatencyMode,
    pub input_gain_db: f32,
    pub output_gain_db: f32,
    pub theme: ThemePref,
    /// Start processing on launch if it was running when the app last closed.
    pub was_running: bool,
    /// The user (or first-run auto-detection) has picked the cable; don't auto-select again.
    pub cable_choice_made: bool,
    /// Slower meter refresh to save CPU.
    pub low_power_ui: bool,
    /// Effect settings (also the format presets use).
    pub fx: FxSettings,
    /// Last loaded preset (shown with "*" once modified).
    pub preset: Option<String>,
    pub ui_mode: UiMode,
    pub hotkeys: HotkeyConfig,
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    pub start_minimized: bool,
    /// The "still running in the tray" hint has been shown once.
    pub tray_hint_shown: bool,
    /// Live spectrum under the level meters.
    pub show_spectrum: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            input: None,
            cable: None,
            monitor: None,
            monitor_enabled: false,
            input_channel: None,
            latency: LatencyMode::Balanced,
            input_gain_db: 0.0,
            output_gain_db: 0.0,
            theme: ThemePref::System,
            was_running: false,
            cable_choice_made: false,
            low_power_ui: false,
            fx: FxSettings::default(),
            preset: Some("Normal".to_string()),
            ui_mode: UiMode::Simple,
            hotkeys: HotkeyConfig::default(),
            close_to_tray: true,
            start_minimized: false,
            tray_hint_shown: false,
            show_spectrum: true,
        }
    }
}

/// `%APPDATA%\VoiceChanger`, created on demand.
pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("VoiceChanger");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => {
                let mut cfg: Self = serde_json::from_str(&text).unwrap_or_else(|e| {
                    log::warn!("config.json unreadable ({e}); using defaults");
                    Self::default()
                });
                // Effects added since this config was written join the chain at their default spot.
                cfg.fx.normalize();
                cfg
            }
            Err(_) => Self::default(),
        }
    }

    /// Write atomically (temp file + rename) so a crash never leaves a half-written config.
    pub fn save(&self) {
        let path = config_path();
        let tmp = path.with_extension("json.tmp");
        let result = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(&tmp, text))
            .and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = result {
            log::error!("failed to save config: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let c: Config = serde_json::from_str(r#"{"monitor_enabled": true}"#).unwrap();
        assert!(c.monitor_enabled);
        assert_eq!(c.latency, LatencyMode::Balanced);
        assert_eq!(c.version, CONFIG_VERSION);
    }
}
