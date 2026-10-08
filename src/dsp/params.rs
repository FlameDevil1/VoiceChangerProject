//! Declarative effect parameters.
//!
//! Each effect describes its controls once in an `EffectSpec` (key, range, default, unit, help).
//! From that single table we get the live atomic values (`EffectParams`), the serialisable
//! settings (`EffectSettings`, used by config and presets), CLI parsing and the GUI sliders.

use super::shared_params::{AtomicF32, SlotParams};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU32;

#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: &'static str,
    pub step: f32,
    pub help: &'static str,
}

impl ParamSpec {
    pub fn clamp(&self, v: f32) -> f32 {
        if v.is_finite() { v.clamp(self.min, self.max) } else { self.default }
    }
}

/// A named combination of parameter values (e.g. reverb "Hall").
pub type Preset = (&'static str, &'static [(&'static str, f32)]);

#[derive(Clone, Copy, Debug)]
pub struct EffectSpec {
    pub label: &'static str,
    pub help: &'static str,
    pub params: &'static [ParamSpec],
    /// What the wet/dry control is called for this effect ("Mix", "Strength", ...).
    pub mix_label: &'static str,
    pub default_mix: f32,
    pub presets: &'static [Preset],
    /// Parameters shown as a dropdown: (key, option labels); the value is the option index.
    pub choices: &'static [(&'static str, &'static [&'static str])],
    /// Parameters the "Randomize" button may change, with the range it picks from.
    pub random: &'static [(&'static str, f32, f32)],
}

impl EffectSpec {
    pub fn index(&self, key: &str) -> Option<usize> {
        self.params.iter().position(|p| p.key == key)
    }

    /// Option labels if `key` is a dropdown parameter.
    pub fn choices_for(&self, key: &str) -> Option<&'static [&'static str]> {
        self.choices.iter().find(|(k, _)| *k == key).map(|(_, c)| *c)
    }
}

/// Live parameters of one effect: written by the UI, read by the audio thread once per block.
#[derive(Debug)]
pub struct EffectParams {
    pub slot: SlotParams,
    values: Box<[AtomicF32]>,
    /// Effect-specific status for the UI (e.g. 1 = unsupported sample rate).
    pub status: AtomicU32,
    /// Effect-specific live readout for the UI (gate gain, gain reduction, voice probability).
    pub meter: AtomicF32,
    spec: &'static EffectSpec,
}

impl EffectParams {
    pub fn new(spec: &'static EffectSpec) -> Self {
        let slot = SlotParams::default();
        slot.mix.store(spec.default_mix);
        Self {
            slot,
            values: spec.params.iter().map(|p| AtomicF32::new(p.default)).collect(),
            status: AtomicU32::new(0),
            meter: AtomicF32::new(0.0),
            spec,
        }
    }

    #[inline]
    pub fn get(&self, i: usize) -> f32 {
        self.values[i].load()
    }

    pub fn set(&self, i: usize, v: f32) {
        self.values[i].store(self.spec.params[i].clamp(v));
    }

    pub fn spec(&self) -> &'static EffectSpec {
        self.spec
    }
}

/// Serialisable settings of one effect. Unknown keys are ignored and missing ones take the spec
/// default, so presets and configs survive parameters being added or removed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectSettings {
    pub enabled: bool,
    /// `None` = the effect's default mix.
    pub mix: Option<f32>,
    pub params: BTreeMap<String, f32>,
}
