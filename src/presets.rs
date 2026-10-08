//! Presets: named effect settings. Built-ins live in code; user presets are one JSON file each in
//! `%APPDATA%\VoiceChanger\presets`, so they are easy to back up, share and import.
//!
//! **Mic cleanup is kept separate from voice character.** Noise suppression and the gate depend
//! on your room and microphone, not on the voice you want, so loading a preset leaves them alone
//! unless the preset was saved with "include mic cleanup".
//!
//! **Problems are a layer too.** The bad mic and bad connection effects are set by scenarios
//! ("Laggy Wi-Fi", ...), so you can be a robot on a laggy call. A preset that doesn't use them
//! leaves them as they are; one saved with them on sets them.

use crate::dsp::{EffectKind, FxSettings};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PRESET_SCHEMA: u32 = 1;

/// Effects that adapt the mic to its environment rather than change the voice.
pub const CLEANUP: [EffectKind; 2] = [EffectKind::Denoise, EffectKind::Gate];

/// Effects that simulate bad hardware and connections, set by scenarios.
pub const PROBLEMS: [EffectKind; 2] = [EffectKind::BadMic, EffectKind::Network];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    /// Lets future versions migrate old files. Unknown fields are ignored, missing ones defaulted.
    pub schema_version: u32,
    pub name: String,
    pub description: String,
    pub fx: FxSettings,
    /// Whether loading this preset also sets noise suppression and the gate.
    pub include_cleanup: bool,
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            schema_version: PRESET_SCHEMA,
            name: String::new(),
            description: String::new(),
            fx: FxSettings::default(),
            include_cleanup: false,
        }
    }
}

impl Preset {
    /// Snapshot the current settings.
    pub fn capture(name: &str, current: &FxSettings, include_cleanup: bool) -> Self {
        let mut fx = current.clone();
        // Store every effect explicitly so the preset restores exactly what was saved.
        for kind in EffectKind::ALL {
            fx.set_enabled(kind, current.enabled(kind));
        }
        Self { name: name.trim().to_string(), fx, include_cleanup, ..Default::default() }
    }

    /// Settings after loading this preset on top of `current`.
    pub fn apply(&self, current: &FxSettings) -> FxSettings {
        let mut out = self.fx.clone();
        out.normalize();
        let keep_problems = !PROBLEMS.iter().any(|&k| self.fx.enabled(k));
        let keep = CLEANUP.iter().filter(|_| !self.include_cleanup).chain(PROBLEMS.iter().filter(|_| keep_problems));
        for &kind in keep {
            match current.effects.get(&kind) {
                Some(s) => out.effects.insert(kind, s.clone()),
                None => out.effects.remove(&kind),
            };
        }
        out
    }

    /// Does `current` still match this preset (ignoring cleanup if it isn't included)?
    pub fn matches(&self, current: &FxSettings) -> bool {
        let applied = self.apply(current);
        EffectKind::ALL.iter().all(|&k| {
            applied.enabled(k) == current.enabled(k)
                && (!current.enabled(k)
                    || (applied.mix(k) == current.mix(k)
                        && k.spec().params.iter().all(|p| applied.get(k, p.key) == current.get(k, p.key))))
        }) && applied.order == current.order
    }
}

/// A bad mic / bad connection combination, applied on top of whatever voice is loaded.
#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    /// Bad mic settings (empty = bad mic off).
    pub bad_mic: &'static [(&'static str, f32)],
    /// Bad connection settings (empty = off).
    pub network: &'static [(&'static str, f32)],
}

impl Scenario {
    /// `current` with this scenario's problems (and nothing else) changed.
    pub fn apply(&self, current: &FxSettings) -> FxSettings {
        let mut out = current.clone();
        for (kind, values) in [(EffectKind::BadMic, self.bad_mic), (EffectKind::Network, self.network)] {
            out.effects.remove(&kind);
            if !values.is_empty() {
                out = out.with(kind, values);
            }
        }
        out
    }

    pub fn matches(&self, current: &FxSettings) -> bool {
        let applied = self.apply(current);
        PROBLEMS.iter().all(|&k| {
            applied.enabled(k) == current.enabled(k)
                && (!current.enabled(k)
                    || k.spec().params.iter().all(|p| applied.get(k, p.key) == current.get(k, p.key)))
        })
    }
}

/// `current` with the bad mic and bad connection turned off.
pub fn clear_problems(current: &FxSettings) -> FxSettings {
    let mut out = current.clone();
    for kind in PROBLEMS {
        out.set_enabled(kind, false);
    }
    out
}

/// Built-in problem scenarios, in display order.
pub const SCENARIOS: [Scenario; 6] = [
    Scenario {
        name: "Cheap headset",
        description: "Thin, hissy, pumping gaming headset",
        bad_mic: &[("low_cut", 250.0), ("high_cut", 5000.0), ("hiss", 35.0), ("clip", 20.0), ("pump", 50.0)],
        network: &[("amount", 15.0), ("lag", 0.0), ("drift", 0.0), ("freeze", 0.0), ("codec_rate", 16000.0)],
    },
    Scenario {
        name: "Laggy Wi-Fi",
        description: "Lag spikes, falling behind and the odd stutter",
        bad_mic: &[],
        network: &[
            ("amount", 55.0),
            ("lag", 80.0),
            ("loss", 40.0),
            ("jitter", 25.0),
            ("choppy", 20.0),
            ("drift", 40.0),
            ("freeze", 10.0),
        ],
    },
    Scenario {
        name: "Tunnel",
        description: "Mobile data in a tunnel: breaking up badly",
        bad_mic: &[("low_cut", 300.0), ("high_cut", 3400.0)],
        network: &[
            ("amount", 85.0),
            ("lag", 50.0),
            ("loss", 80.0),
            ("jitter", 70.0),
            ("choppy", 60.0),
            ("drift", 10.0),
            ("freeze", 40.0),
            ("bits", 10.0),
            ("codec_rate", 8000.0),
            ("variation", 30.0),
        ],
    },
    Scenario {
        name: "Broken cable",
        description: "Crackle, hum and cut-outs from a loose connection",
        bad_mic: &[("crackle", 70.0), ("dropout", 50.0), ("hum", 40.0), ("handling", 20.0), ("variation", 70.0)],
        network: &[],
    },
    Scenario {
        name: "Old webcam",
        description: "Distant, hissy built-in webcam mic",
        bad_mic: &[
            ("low_cut", 200.0),
            ("high_cut", 6000.0),
            ("hiss", 55.0),
            ("room", 45.0),
            ("pump", 70.0),
            ("drift", 20.0),
        ],
        network: &[("amount", 10.0), ("lag", 0.0), ("drift", 0.0), ("codec_rate", 16000.0)],
    },
    Scenario {
        name: "Bathroom speakerphone",
        description: "Phone on speaker in a small, echoey room",
        bad_mic: &[("room", 90.0), ("low_cut", 400.0), ("high_cut", 4500.0), ("pump", 40.0), ("clip", 15.0)],
        network: &[("amount", 20.0), ("lag", 10.0), ("drift", 0.0), ("codec_rate", 16000.0)],
    },
];

fn builtin(name: &str, description: &str, fx: FxSettings) -> Preset {
    Preset { name: name.into(), description: description.into(), fx, ..Default::default() }
}

/// Built-in presets, in display order. Every built-in is a voice preset: cleanup is left alone.
pub fn builtins() -> Vec<Preset> {
    use EffectKind::*;
    let none = FxSettings::default;
    vec![
        builtin("Normal", "Your own voice, no effects", none()),
        builtin(
            "Deep voice",
            "Lower and bigger",
            none()
                .with(Pitch, &[("semitones", -4.0), ("formant", -3.0)])
                .with(Eq, &[("low", 2.0), ("low_mid", 1.0)])
                .with(Compressor, &[("threshold", -18.0), ("ratio", 2.0), ("makeup", 2.0)]),
        ),
        builtin(
            "Monster",
            "Huge, growling creature",
            none()
                .with(Pitch, &[("semitones", -12.0), ("formant", -6.0)])
                .with(Eq, &[("low", 4.0), ("high", -3.0)])
                .with(Reverb, &[("size", 30.0), ("decay", 0.8), ("damping", 60.0)])
                .with_mix(Reverb, 0.2),
        ),
        builtin(
            "Female",
            "Male to female",
            none().with(Pitch, &[("semitones", 6.0), ("formant", 3.0)]).with(Eq, &[("low", -2.0), ("high_mid", 2.0)]),
        ),
        builtin(
            "Male",
            "Female to male",
            none().with(Pitch, &[("semitones", -6.0), ("formant", -3.0)]).with(Eq, &[("low", 2.0)]),
        ),
        builtin("Child", "Small and young", none().with(Pitch, &[("semitones", 8.0), ("formant", 5.0)])),
        builtin("Chipmunk", "Helium squeak", none().with(Pitch, &[("semitones", 8.0), ("formant", 8.0)])),
        builtin("Robot", "Monotone and metallic", none().with(Robot, &[]).with(Eq, &[("high_mid", 2.0)])),
        builtin(
            "Alien",
            "Otherworldly and warbling",
            none()
                .with(Pitch, &[("semitones", 3.0), ("formant", -4.0)])
                .with(Robot, &[("monotone", 40.0), ("ring", 60.0), ("ring_hz", 90.0), ("metallic", 20.0)])
                .with(Reverb, &[("size", 60.0), ("decay", 2.0)])
                .with_mix(Reverb, 0.2),
        ),
        builtin(
            "Telephone",
            "Old phone line",
            none().with(Radio, &[("low_cut", 300.0), ("high_cut", 3400.0), ("drive", 20.0)]),
        ),
        builtin(
            "Walkie-talkie",
            "Crunchy two-way radio",
            none().with(Radio, &[("low_cut", 700.0), ("high_cut", 2800.0), ("drive", 70.0), ("noise", 30.0)]),
        ),
        builtin(
            "Cave",
            "Huge echoing cave",
            none()
                .with(Reverb, &[("size", 100.0), ("decay", 6.0), ("damping", 25.0), ("predelay", 40.0)])
                .with_mix(Reverb, 0.4),
        ),
        builtin(
            "Announcer",
            "Big, polished stadium voice",
            none()
                .with(Eq, &[("low", 2.0), ("high_mid", 2.0)])
                .with(Compressor, &[("threshold", -24.0), ("ratio", 4.0), ("attack", 3.0), ("makeup", 8.0)])
                .with(Reverb, &[("size", 75.0), ("decay", 2.8), ("predelay", 25.0)])
                .with_mix(Reverb, 0.2),
        ),
        builtin(
            "Podcast",
            "Clear, even broadcast voice",
            none()
                .with(Eq, &[("low", -3.0), ("low_mid", -2.0), ("high_mid", 3.0), ("high", 1.0)])
                .with(Compressor, &[("threshold", -24.0), ("ratio", 4.0), ("attack", 3.0), ("makeup", 9.0)]),
        ),
        builtin(
            "Old man",
            "Shaky, weathered voice",
            none()
                .with(Pitch, &[("semitones", -2.0), ("formant", -1.0), ("vibrato", 18.0), ("vibrato_rate", 6.5)])
                .with(Character, &[("tone", -25.0), ("breath", 30.0), ("rough", 45.0)]),
        ),
        builtin(
            "Ghost",
            "Breathy, doubled and distant",
            none()
                .with(Pitch, &[("semitones", 1.0), ("intonation", 60.0)])
                .with(Character, &[("tone", 20.0), ("breath", 70.0), ("double", 60.0), ("presence", -30.0)])
                .with(Reverb, &[("size", 75.0), ("decay", 2.8), ("damping", 40.0), ("predelay", 25.0)])
                .with_mix(Reverb, 0.35),
        ),
        builtin(
            "Auto-tune",
            "Hard pitch correction, pop style",
            none()
                .with(Pitch, &[("autotune", 100.0)])
                .with(Compressor, &[("threshold", -18.0), ("ratio", 2.0), ("makeup", 2.0)])
                .with(Reverb, &[("size", 25.0), ("decay", 0.6), ("damping", 60.0), ("predelay", 5.0)])
                .with_mix(Reverb, 0.15),
        ),
    ]
}

/// Where a preset lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    User(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub preset: Preset,
    pub source: Source,
}

/// Built-in plus user presets.
pub struct PresetStore {
    dir: PathBuf,
    entries: Vec<Entry>,
}

impl PresetStore {
    pub fn default_dir() -> PathBuf {
        crate::config::app_dir().join("presets")
    }

    /// Load built-ins and every readable `*.json` in `dir` (bad files are skipped and logged).
    pub fn load(dir: &Path) -> Self {
        let mut store = Self { dir: dir.to_path_buf(), entries: Vec::new() };
        store.reload();
        store
    }

    pub fn reload(&mut self) {
        self.entries = builtins().into_iter().map(|preset| Entry { preset, source: Source::BuiltIn }).collect();
        let mut user: Vec<Entry> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
            .filter_map(|path| match read_preset(&path) {
                Ok(preset) => Some(Entry { preset, source: Source::User(path) }),
                Err(e) => {
                    log::warn!("skipping preset {}: {e}", path.display());
                    None
                }
            })
            .collect();
        user.sort_by_key(|e| e.preset.name.to_lowercase());
        self.entries.extend(user);
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.preset.name == name)
    }

    /// Next/previous preset name after `current`, wrapping (for hotkeys and the tray).
    pub fn neighbour(&self, current: Option<&str>, forward: bool) -> Option<&str> {
        let n = self.entries.len();
        if n == 0 {
            return None;
        }
        let i = current.and_then(|c| self.entries.iter().position(|e| e.preset.name == c));
        let next = match (i, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        Some(&self.entries[next].preset.name)
    }

    fn validate_name(&self, name: &str, except: Option<&str>) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Enter a name.".into());
        }
        if name.chars().count() > 60 {
            return Err("Names can be at most 60 characters.".into());
        }
        if let Some(e) = self.find(name)
            && Some(name) != except
        {
            return Err(match e.source {
                Source::BuiltIn => format!("\"{name}\" is a built-in preset name."),
                Source::User(_) => format!("A preset called \"{name}\" already exists."),
            });
        }
        Ok(name.to_string())
    }

    /// Save a new user preset (or overwrite the user preset of the same name if `overwrite`).
    pub fn save(&mut self, mut preset: Preset, overwrite: bool) -> Result<(), String> {
        let existing = self.find(preset.name.trim()).cloned();
        let name = match &existing {
            Some(Entry { source: Source::User(_), .. }) if overwrite => preset.name.trim().to_string(),
            _ => self.validate_name(&preset.name, None)?,
        };
        preset.name = name;
        preset.schema_version = PRESET_SCHEMA;
        let path = match existing {
            Some(Entry { source: Source::User(p), .. }) if overwrite => p,
            _ => self.unique_path(&preset.name),
        };
        write_preset(&path, &preset)?;
        self.reload();
        Ok(())
    }

    pub fn rename(&mut self, old: &str, new: &str) -> Result<(), String> {
        let Some(Entry { preset, source: Source::User(path) }) = self.find(old).cloned() else {
            return Err("Only your own presets can be renamed.".into());
        };
        let name = self.validate_name(new, Some(old))?;
        let new_path = if name.eq_ignore_ascii_case(old) { path.clone() } else { self.unique_path(&name) };
        write_preset(&new_path, &Preset { name, ..preset })?;
        if new_path != path {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        self.reload();
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let Some(Entry { source: Source::User(path), .. }) = self.find(name).cloned() else {
            return Err("Built-in presets can't be deleted.".into());
        };
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    /// Import a preset file. A name clash gets " (2)", " (3)", ... appended. Returns the name.
    pub fn import(&mut self, path: &Path) -> Result<String, String> {
        let mut preset = read_preset(path)?;
        if preset.name.trim().is_empty() {
            preset.name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported").to_string();
        }
        self.add_unique(preset)
    }

    /// Save as a new user preset, renaming to "Name (2)", "Name (3)", ... on a clash.
    pub fn add_unique(&mut self, mut preset: Preset) -> Result<String, String> {
        let base = match preset.name.trim() {
            "" => "Preset".to_string(),
            n => n.to_string(),
        };
        preset.name = base.clone();
        let mut n = 2;
        while self.validate_name(&preset.name, None).is_err() {
            preset.name = format!("{base} ({n})");
            n += 1;
        }
        let name = preset.name.clone();
        self.save(preset, false)?;
        Ok(name)
    }

    pub fn export(&self, name: &str, path: &Path) -> Result<(), String> {
        let e = self.find(name).ok_or("No such preset.")?;
        write_preset(path, &e.preset)
    }

    fn unique_path(&self, name: &str) -> PathBuf {
        let stem: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
            .collect::<String>()
            .trim()
            .to_string();
        let stem = if stem.is_empty() { "preset".to_string() } else { stem };
        let mut path = self.dir.join(format!("{stem}.json"));
        let mut n = 2;
        while path.exists() {
            path = self.dir.join(format!("{stem} ({n}).json"));
            n += 1;
        }
        path
    }
}

fn read_preset(path: &Path) -> Result<Preset, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut p: Preset =
        serde_json::from_str(&text).map_err(|e| format!("{}: not a valid preset ({e})", path.display()))?;
    p.fx.normalize();
    Ok(p)
}

fn write_preset(path: &Path, preset: &Preset) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(preset).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::CoreParams;
    use crate::offline::{self, analysis, signals};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vc-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn builtins_are_valid_unique_and_render_cleanly() {
        let all = builtins();
        let mut names: Vec<_> = all.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len(), "duplicate built-in names");
        let x = signals::vowel(48_000, 0.5, 150.0);
        for p in &all {
            assert!(!p.include_cleanup, "{}: built-ins must not touch cleanup", p.name);
            for (kind, s) in &p.fx.effects {
                for key in s.params.keys() {
                    assert!(kind.spec().index(key).is_some(), "{}: {kind:?} has no param {key}", p.name);
                }
            }
            let y = offline::render(&x, 48_000, CoreParams::default(), &p.fx, 480);
            assert!(y.iter().all(|s| s.is_finite()) && analysis::peak(&y) <= 0.9, "{}", p.name);
        }
    }

    /// Clicking through voices and effect presets shouldn't make you suddenly loud or quiet.
    #[test]
    fn presets_keep_roughly_the_same_loudness() {
        let mut x = signals::vowel_wobble(48_000, 1.5, 140.0, 0.15, 2.0);
        x.extend(signals::silence(48_000, 0.3));
        x.extend(signals::vowel_wobble(48_000, 1.0, 180.0, 0.1, 3.0).iter().map(|s| s * 0.5));
        let change = |fx: &FxSettings| {
            analysis::rms_db(&offline::render(&x, 48_000, CoreParams::default(), fx, 480)) - analysis::rms_db(&x)
        };
        for p in builtins() {
            let d = change(&p.fx);
            assert!(d.abs() < 3.0, "voice {}: {d:+.1} dB", p.name);
        }
        for kind in EffectKind::ALL {
            for (name, values) in kind.spec().presets {
                let d = change(&FxSettings::default().with(kind, values));
                assert!(d.abs() < 3.0, "{} preset {name}: {d:+.1} dB", kind.key());
            }
        }
        for s in SCENARIOS {
            let d = change(&s.apply(&FxSettings::default()));
            assert!(d.abs() < 3.0, "scenario {}: {d:+.1} dB", s.name);
        }
    }

    #[test]
    fn scenarios_are_valid_and_layer_on_top_of_the_voice() {
        let mut names: Vec<_> = SCENARIOS.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SCENARIOS.len());
        for s in SCENARIOS {
            assert!(!s.bad_mic.is_empty() || !s.network.is_empty(), "{}", s.name);
            for (kind, values) in [(EffectKind::BadMic, s.bad_mic), (EffectKind::Network, s.network)] {
                for (key, v) in values {
                    let p = kind.spec().index(key).map(|i| kind.spec().params[i]);
                    assert!(p.is_some_and(|p| (p.min..=p.max).contains(v)), "{}: {kind:?} {key}", s.name);
                }
            }
        }
        let robot = builtins().into_iter().find(|p| p.name == "Robot").unwrap();
        let wifi = SCENARIOS[1];
        // Scenario on top of a voice: the voice stays.
        let fx = wifi.apply(&robot.apply(&FxSettings::default()));
        assert!(robot.matches(&fx) && wifi.matches(&fx));
        assert!(fx.enabled(EffectKind::Network) && !fx.enabled(EffectKind::BadMic));
        // Another voice on top of the scenario: the scenario stays.
        let deep = builtins().into_iter().find(|p| p.name == "Deep voice").unwrap();
        let fx = deep.apply(&fx);
        assert!(deep.matches(&fx) && wifi.matches(&fx) && !robot.matches(&fx));
        // Clearing turns problems off and keeps the voice.
        let fx = clear_problems(&fx);
        assert!(deep.matches(&fx) && !wifi.matches(&fx));
        assert!(PROBLEMS.iter().all(|&k| !fx.enabled(k)));
        // A preset saved with a problem on sets it when loaded.
        let saved = Preset::capture("Laggy robot", &wifi.apply(&robot.apply(&FxSettings::default())), false);
        let fx = saved.apply(&SCENARIOS[3].apply(&FxSettings::default()));
        assert!(wifi.matches(&fx), "the saved scenario replaces the current one");
    }

    #[test]
    fn loading_a_voice_preset_keeps_mic_cleanup() {
        let current = FxSettings::default()
            .with(EffectKind::Denoise, &[])
            .with(EffectKind::Gate, &[("threshold", -50.0)])
            .with(EffectKind::Reverb, &[]);
        let robot = builtins().into_iter().find(|p| p.name == "Robot").unwrap();
        let out = robot.apply(&current);
        assert!(out.enabled(EffectKind::Denoise) && out.enabled(EffectKind::Gate));
        assert_eq!(out.get(EffectKind::Gate, "threshold"), -50.0);
        assert!(out.enabled(EffectKind::Robot));
        assert!(!out.enabled(EffectKind::Reverb), "effects not in the preset are turned off");
        assert!(robot.matches(&out));
        // A preset saved with cleanup restores it exactly.
        let saved = Preset::capture("Mine", &FxSettings::default().with(EffectKind::Pitch, &[]), true);
        let out = saved.apply(&current);
        assert!(!out.enabled(EffectKind::Denoise) && !out.enabled(EffectKind::Gate));
    }

    #[test]
    fn save_rename_delete_import_export_roundtrip() {
        let dir = temp_dir("crud");
        let mut store = PresetStore::load(&dir);
        let builtin_count = store.entries().len();
        let fx = FxSettings::default().with(EffectKind::Pitch, &[("semitones", -3.0)]);

        store.save(Preset::capture("My voice", &fx, false), false).unwrap();
        assert_eq!(store.entries().len(), builtin_count + 1);
        assert!(store.save(Preset::capture("My voice", &fx, false), false).is_err(), "duplicate name");
        assert!(store.save(Preset::capture("Robot", &fx, false), false).is_err(), "built-in name");
        assert!(store.save(Preset::capture("  ", &fx, false), false).is_err(), "empty name");
        // Overwrite updates in place.
        let fx2 = FxSettings::default().with(EffectKind::Pitch, &[("semitones", 5.0)]);
        store.save(Preset::capture("My voice", &fx2, false), true).unwrap();
        assert_eq!(store.find("My voice").unwrap().preset.fx.get(EffectKind::Pitch, "semitones"), 5.0);

        store.rename("My voice", "Deep/er: v2").unwrap();
        assert!(store.find("My voice").is_none());
        assert!(store.find("Deep/er: v2").is_some());
        assert!(store.rename("Robot", "X").is_err(), "built-ins can't be renamed");

        let out = dir.join("exported.json.out");
        store.export("Deep/er: v2", &out).unwrap();
        let imported = store.import(&out).unwrap();
        assert_eq!(imported, "Deep/er: v2 (2)");
        assert_eq!(store.find(&imported).unwrap().preset.fx.get(EffectKind::Pitch, "semitones"), 5.0);

        store.delete("Deep/er: v2").unwrap();
        store.delete(&imported).unwrap();
        assert!(store.delete("Robot").is_err());
        assert_eq!(store.entries().len(), builtin_count);

        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        store.reload();
        assert_eq!(store.entries().len(), builtin_count, "broken files are skipped");
        assert!(store.import(&dir.join("broken.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neighbour_wraps() {
        let store = PresetStore::load(&temp_dir("nb"));
        let first = store.entries()[0].preset.name.clone();
        let last = store.entries().last().unwrap().preset.name.clone();
        assert_eq!(store.neighbour(Some(&last), true), Some(first.as_str()));
        assert_eq!(store.neighbour(Some(&first), false), Some(last.as_str()));
        assert_eq!(store.neighbour(None, true), Some(first.as_str()));
    }
}
