//! Effect chain: an ordered list of slots, each wrapping one effect with on/off and wet/dry.
//!
//! - **Disabled slots cost nothing and add no latency**: once the fade-out finishes the effect is
//!   skipped entirely (and reset, so re-enabling starts clean).
//! - **Wet/dry mix is latency-compensated**: the dry signal is delayed by the effect's
//!   algorithmic latency so a partial mix never comb-filters.
//! - **On/off toggles are 20 ms crossfades** between the slot's input and output. They are short
//!   enough that the brief latency mismatch during the fade is inaudible, and they let disabled
//!   effects drop out of the latency budget entirely.
//!
//! Parameters live in atomics (`FxParams`), written by the UI and read once per block.
//! `FxSettings` is the plain, serialisable mirror used for config and presets. Both are driven
//! by each effect's `EffectSpec`, so adding an effect means writing its module and one line here.

use super::Processor;
use super::fx;
use super::params::{EffectParams, EffectSettings, EffectSpec};
use super::util::{DelayLine, SmoothedValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const FADE_SECONDS: f32 = 0.020;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    Denoise,
    Gate,
    Pitch,
    Robot,
    Eq,
    Compressor,
    Reverb,
    Radio,
}

impl EffectKind {
    /// Every effect, in the default processing order: clean up the input first (noise removal
    /// before the gate, so the gate sees a clean signal), then change the voice, shape its tone
    /// and level, and finally place it in a space or through a "device".
    pub const ALL: [EffectKind; 8] = [
        EffectKind::Denoise,
        EffectKind::Gate,
        EffectKind::Pitch,
        EffectKind::Robot,
        EffectKind::Eq,
        EffectKind::Compressor,
        EffectKind::Reverb,
        EffectKind::Radio,
    ];

    pub fn spec(self) -> &'static EffectSpec {
        match self {
            EffectKind::Denoise => &fx::denoise::SPEC,
            EffectKind::Gate => &fx::gate::SPEC,
            EffectKind::Pitch => &fx::pitch_fx::SPEC,
            EffectKind::Robot => &fx::robot::SPEC,
            EffectKind::Eq => &fx::eq::SPEC,
            EffectKind::Compressor => &fx::compressor::SPEC,
            EffectKind::Reverb => &fx::reverb::SPEC,
            EffectKind::Radio => &fx::radio::SPEC,
        }
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }

    /// Short lowercase name used on the command line and in presets ("reverb", "eq", ...).
    pub fn key(self) -> &'static str {
        match self {
            EffectKind::Denoise => "denoise",
            EffectKind::Gate => "gate",
            EffectKind::Pitch => "pitch",
            EffectKind::Robot => "robot",
            EffectKind::Eq => "eq",
            EffectKind::Compressor => "compressor",
            EffectKind::Reverb => "reverb",
            EffectKind::Radio => "radio",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    fn make(self, params: Arc<EffectParams>) -> Box<dyn Processor> {
        match self {
            EffectKind::Denoise => Box::new(fx::denoise::Denoise::new(params)),
            EffectKind::Gate => Box::new(fx::gate::Gate::new(params)),
            EffectKind::Pitch => Box::new(fx::pitch_fx::PitchFx::new(params)),
            EffectKind::Robot => Box::new(fx::robot::Robot::new(params)),
            EffectKind::Eq => Box::new(fx::eq::Eq::new(params)),
            EffectKind::Compressor => Box::new(fx::compressor::Compressor::new(params)),
            EffectKind::Reverb => Box::new(fx::reverb::Reverb::new(params)),
            EffectKind::Radio => Box::new(fx::radio::Radio::new(params)),
        }
    }
}

// ---- serialisable settings ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FxSettings {
    /// Processing order. Effects not listed are not in the chain.
    pub order: Vec<EffectKind>,
    pub effects: BTreeMap<EffectKind, EffectSettings>,
}

impl Default for FxSettings {
    fn default() -> Self {
        Self { order: EffectKind::ALL.to_vec(), effects: BTreeMap::new() }
    }
}

impl FxSettings {
    /// Builder for tests, CLI and presets: enable `kind` with the given parameter values.
    pub fn with(mut self, kind: EffectKind, values: &[(&str, f32)]) -> Self {
        self.set_enabled(kind, true);
        for (k, v) in values {
            self.set(kind, k, *v);
        }
        self
    }

    /// Builder: set an effect's wet/dry mix.
    pub fn with_mix(mut self, kind: EffectKind, mix: f32) -> Self {
        self.set_mix(kind, mix);
        self
    }

    pub fn enabled(&self, kind: EffectKind) -> bool {
        self.effects.get(&kind).is_some_and(|e| e.enabled)
    }

    pub fn set_enabled(&mut self, kind: EffectKind, on: bool) {
        self.effects.entry(kind).or_default().enabled = on;
    }

    pub fn mix(&self, kind: EffectKind) -> f32 {
        self.effects.get(&kind).and_then(|e| e.mix).unwrap_or(kind.spec().default_mix)
    }

    pub fn set_mix(&mut self, kind: EffectKind, mix: f32) {
        self.effects.entry(kind).or_default().mix = Some(mix.clamp(0.0, 1.0));
    }

    /// Parameter value (spec default if never set). Unknown keys return 0.
    pub fn get(&self, kind: EffectKind, key: &str) -> f32 {
        let spec = kind.spec();
        let Some(p) = spec.params.iter().find(|p| p.key == key) else { return 0.0 };
        self.effects.get(&kind).and_then(|e| e.params.get(key)).map_or(p.default, |v| p.clamp(*v))
    }

    /// Set a parameter (clamped to its range). Returns false for an unknown key.
    pub fn set(&mut self, kind: EffectKind, key: &str, value: f32) -> bool {
        let Some(p) = kind.spec().params.iter().find(|p| p.key == key) else { return false };
        self.effects.entry(kind).or_default().params.insert(key.to_string(), p.clamp(value));
        true
    }

    /// Make `order` contain every effect exactly once. Effects missing from an older config are
    /// inserted at their default position relative to the ones already listed.
    pub fn normalize(&mut self) {
        let mut seen = Vec::new();
        self.order.retain(|k| {
            let dup = seen.contains(k);
            seen.push(*k);
            !dup
        });
        for (i, kind) in EffectKind::ALL.iter().enumerate() {
            if self.order.contains(kind) {
                continue;
            }
            // Insert after the closest preceding effect (in default order) that is present.
            let pos = EffectKind::ALL[..i]
                .iter()
                .rev()
                .find_map(|prev| self.order.iter().position(|k| k == prev))
                .map_or(0, |p| p + 1);
            self.order.insert(pos, *kind);
        }
    }
}

// ---- live (atomic) parameters ---------------------------------------------------------------

/// One parameter block per effect kind, shared with the processors via `Arc`.
#[derive(Debug)]
pub struct FxParams {
    params: Vec<Arc<EffectParams>>,
}

impl Default for FxParams {
    fn default() -> Self {
        Self { params: EffectKind::ALL.iter().map(|k| Arc::new(EffectParams::new(k.spec()))).collect() }
    }
}

impl FxParams {
    pub fn from_settings(s: &FxSettings) -> Self {
        let p = Self::default();
        p.store(s);
        p
    }

    pub fn get(&self, kind: EffectKind) -> &Arc<EffectParams> {
        &self.params[kind.index()]
    }

    /// Publish settings to the audio thread (a few dozen atomic stores).
    pub fn store(&self, s: &FxSettings) {
        for kind in EffectKind::ALL {
            let p = self.get(kind);
            p.slot.set_enabled(s.enabled(kind));
            p.slot.mix.store(s.mix(kind));
            for (i, spec) in kind.spec().params.iter().enumerate() {
                p.set(i, s.get(kind, spec.key));
            }
        }
    }
}

// ---- slots and chain ------------------------------------------------------------------------

pub struct Slot {
    kind: EffectKind,
    fx: Box<dyn Processor>,
    params: Arc<EffectParams>,
    enable: SmoothedValue,
    mix: SmoothedValue,
    dry_delay: DelayLine,
    input: Vec<f32>,
    is_reset: bool,
}

impl Slot {
    fn new(kind: EffectKind, fx_params: &FxParams, sample_rate: f32, max_block: usize) -> Self {
        let params = fx_params.get(kind).clone();
        let mut fx = kind.make(params.clone());
        fx.prepare(sample_rate, max_block);
        // Start in the current on/off state (a chain built while an effect is on starts on).
        let on = if params.slot.enabled() { 1.0 } else { 0.0 };
        Self {
            kind,
            dry_delay: DelayLine::with_capacity(fx.max_latency(), fx.latency()),
            fx,
            enable: SmoothedValue::new(on, sample_rate, FADE_SECONDS),
            mix: SmoothedValue::new(params.slot.mix.load(), sample_rate, FADE_SECONDS),
            params,
            input: vec![0.0; max_block],
            is_reset: false,
        }
    }

    pub fn kind(&self) -> EffectKind {
        self.kind
    }

    /// True when fully off: skipped, zero CPU, zero latency.
    pub fn is_off(&self) -> bool {
        self.enable.is_settled() && self.enable.current() == 0.0
    }

    fn process(&mut self, buf: &mut [f32]) {
        let slot = &self.params.slot;
        self.enable.set_target(if slot.enabled() { 1.0 } else { 0.0 });
        self.mix.set_target(slot.mix.load().clamp(0.0, 1.0));
        if self.is_off() {
            if !self.is_reset {
                self.fx.reset();
                self.dry_delay.reset();
                self.is_reset = true;
            }
            self.mix.skip(buf.len());
            return;
        }
        self.is_reset = false;

        let input = &mut self.input[..buf.len()];
        input.copy_from_slice(buf);
        self.fx.process(buf);
        // Some effects change latency at runtime (noise suppression's framing mode).
        self.dry_delay.set_delay(self.fx.latency());

        let fully_wet = self.mix.is_settled() && self.mix.current() == 1.0;
        let fully_on = self.enable.is_settled() && self.enable.current() == 1.0;
        for (y, &x) in buf.iter_mut().zip(input.iter()) {
            let dry = self.dry_delay.process(x);
            let m = self.mix.next_value();
            let wet = if fully_wet { *y } else { dry + (*y - dry) * m };
            let e = self.enable.next_value();
            *y = if fully_on { wet } else { x + (wet - x) * e };
        }
    }

    fn latency(&self) -> usize {
        if self.is_off() { 0 } else { self.fx.latency() }
    }
}

pub struct Chain {
    slots: Vec<Slot>,
}

impl Chain {
    pub fn empty() -> Self {
        Self { slots: Vec::new() }
    }

    /// Build and prepare a chain. Allocates: call off the audio thread.
    pub fn build(order: &[EffectKind], fx: &FxParams, sample_rate: f32, max_block: usize) -> Self {
        let mut seen = Vec::new();
        let slots = order
            .iter()
            .filter(|k| {
                let dup = seen.contains(*k);
                seen.push(**k);
                !dup
            })
            .map(|&k| Slot::new(k, fx, sample_rate, max_block))
            .collect();
        Self { slots }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn order(&self) -> Vec<EffectKind> {
        self.slots.iter().map(|s| s.kind()).collect()
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for slot in &mut self.slots {
            slot.process(buf);
        }
    }

    /// Current latency: sum over slots that are on.
    pub fn latency(&self) -> usize {
        self.slots.iter().map(|s| s.latency()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    fn pitch(enabled: bool, st: f32, mix: f32) -> FxSettings {
        let mut s = FxSettings::default().with(EffectKind::Pitch, &[("semitones", st)]);
        s.set_enabled(EffectKind::Pitch, enabled);
        s.set_mix(EffectKind::Pitch, mix);
        s
    }

    fn run(chain: &mut Chain, x: &[f32]) -> Vec<f32> {
        let mut y = x.to_vec();
        for c in y.chunks_mut(480) {
            chain.process(c);
        }
        y
    }

    #[test]
    fn disabled_effect_is_exact_passthrough_with_zero_latency() {
        let fx = FxParams::from_settings(&pitch(false, 7.0, 1.0));
        let mut chain = Chain::build(&EffectKind::ALL, &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 0.2, 150.0);
        assert_eq!(run(&mut chain, &x), x);
        assert_eq!(chain.latency(), 0);
    }

    #[test]
    fn zero_mix_is_delayed_dry() {
        let fx = FxParams::from_settings(&pitch(true, 7.0, 0.0));
        let mut chain = Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 0.2, 150.0);
        let y = run(&mut chain, &x);
        let d = chain.latency();
        assert_eq!(d, 3);
        assert_eq!(&y[d..], &x[..x.len() - d]);
    }

    #[test]
    fn toggling_does_not_click() {
        let fx = FxParams::from_settings(&pitch(false, 5.0, 1.0));
        let mut chain = Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 1.2, 150.0);
        let mut y = x.clone();
        let slot = &fx.get(EffectKind::Pitch).slot;
        for (i, c) in y.chunks_mut(480).enumerate() {
            // On at 0.3 s, off at 0.8 s.
            slot.set_enabled((30..80).contains(&i));
            chain.process(c);
        }
        let max_in = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        let max_out = y.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_out < max_in * 2.0, "in {max_in}, out {max_out}");
        assert!(chain.slots[0].is_off());
        assert!(analysis::rms(&y[45_000..55_000]) > 0.05);
    }

    #[test]
    fn duplicate_kinds_are_ignored() {
        let fx = FxParams::default();
        let chain = Chain::build(&[EffectKind::Pitch, EffectKind::Pitch], &fx, 48_000.0, 64);
        assert_eq!(chain.order(), vec![EffectKind::Pitch]);
    }

    #[test]
    fn settings_defaults_clamping_and_keys() {
        let mut s = FxSettings::default();
        assert_eq!(s.get(EffectKind::Reverb, "decay"), 1.5);
        assert!(s.set(EffectKind::Reverb, "decay", 99.0));
        assert_eq!(s.get(EffectKind::Reverb, "decay"), 10.0);
        assert!(!s.set(EffectKind::Reverb, "nope", 1.0));
        assert_eq!(s.mix(EffectKind::Reverb), 0.25);
        for k in EffectKind::ALL {
            assert_eq!(EffectKind::from_key(k.key()), Some(k));
            let keys: Vec<_> = k.spec().params.iter().map(|p| p.key).collect();
            let mut dedup = keys.clone();
            dedup.sort();
            dedup.dedup();
            assert_eq!(keys.len(), dedup.len(), "{k:?} has duplicate keys");
            for p in k.spec().params {
                assert!(p.min <= p.default && p.default <= p.max, "{k:?}.{}", p.key);
            }
            for (name, values) in k.spec().presets {
                for (key, v) in *values {
                    let p = k.spec().params.iter().find(|p| p.key == *key);
                    assert!(p.is_some_and(|p| (p.min..=p.max).contains(v)), "{k:?} preset {name}: {key}");
                }
            }
        }
    }

    #[test]
    fn normalize_inserts_missing_effects_in_place() {
        let mut s = FxSettings { order: vec![EffectKind::Pitch, EffectKind::Reverb], ..Default::default() };
        s.normalize();
        assert_eq!(s.order, EffectKind::ALL.to_vec());
        // A custom order is kept; new effects slot in after their predecessor.
        let mut s = FxSettings { order: vec![EffectKind::Reverb, EffectKind::Pitch], ..Default::default() };
        s.normalize();
        assert_eq!(s.order.len(), EffectKind::ALL.len());
        let pos = |k| s.order.iter().position(|x| *x == k).unwrap();
        assert!(pos(EffectKind::Reverb) < pos(EffectKind::Pitch));
        assert_eq!(pos(EffectKind::Robot), pos(EffectKind::Pitch) + 1);
    }

    #[test]
    fn settings_roundtrip_through_json() {
        let s = FxSettings::default()
            .with(EffectKind::Reverb, &[("decay", 2.5)])
            .with(EffectKind::Pitch, &[("semitones", -4.0)]);
        let json = serde_json::to_string(&s).unwrap();
        let back: FxSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.get(EffectKind::Reverb, "decay"), 2.5);
    }
}
