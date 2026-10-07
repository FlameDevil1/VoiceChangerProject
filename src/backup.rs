//! Settings backup: config and user presets in one JSON file, for moving to another PC or
//! recovering after a reinstall.

use crate::config::Config;
use crate::presets::{Preset, PresetStore, Source};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BACKUP_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Backup {
    pub schema_version: u32,
    pub app_version: String,
    pub config: Config,
    /// User presets only (built-ins ship with the app).
    pub presets: Vec<Preset>,
}

impl Backup {
    pub fn capture(config: &Config, store: &PresetStore) -> Self {
        Self {
            schema_version: BACKUP_SCHEMA,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            config: config.clone(),
            presets: store
                .entries()
                .iter()
                .filter(|e| matches!(e.source, Source::User(_)))
                .map(|e| e.preset.clone())
                .collect(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut b: Backup =
            serde_json::from_str(&text).map_err(|e| format!("{}: not a Voice Changer backup ({e})", path.display()))?;
        if b.schema_version == 0 {
            return Err(format!("{}: not a Voice Changer backup", path.display()));
        }
        b.config.fx.normalize();
        Ok(b)
    }

    /// Add the backed-up presets to `store` (name clashes get " (2)" etc.). Returns how many.
    pub fn restore_presets(&self, store: &mut PresetStore) -> Result<usize, String> {
        for p in &self.presets {
            store.add_unique(p.clone())?;
        }
        Ok(self.presets.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{EffectKind, FxSettings};

    #[test]
    fn backup_roundtrip_restores_config_and_presets() {
        let dir = std::env::temp_dir().join(format!("vc-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = PresetStore::load(&dir.join("a"));
        let fx = FxSettings::default().with(EffectKind::Robot, &[("pitch_hz", 150.0)]);
        store.save(Preset::capture("Bot", &fx, false), false).unwrap();
        let cfg = Config { output_gain_db: -3.5, fx: fx.clone(), ..Default::default() };

        let file = dir.join("backup.json");
        Backup::capture(&cfg, &store).save(&file).unwrap();
        let b = Backup::load(&file).unwrap();
        assert_eq!(b.config.output_gain_db, -3.5);
        assert_eq!(b.config.fx.get(EffectKind::Robot, "pitch_hz"), 150.0);
        assert_eq!(b.presets.len(), 1, "only user presets are backed up");

        let mut other = PresetStore::load(&dir.join("b"));
        assert_eq!(b.restore_presets(&mut other).unwrap(), 1);
        assert!(other.find("Bot").is_some());
        // Restoring again doesn't overwrite: the copy gets a new name.
        b.restore_presets(&mut other).unwrap();
        assert!(other.find("Bot (2)").is_some());

        std::fs::write(dir.join("junk.json"), "{}").unwrap();
        assert!(Backup::load(&dir.join("junk.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
