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
//! `FxSettings` is the plain, serialisable mirror used for config and presets.

use super::pitch::{PitchParams, PitchShifter};
use super::shared_params::SlotParams;
use super::util::{DelayLine, SmoothedValue};
use super::Processor;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

const FADE_SECONDS: f32 = 0.020;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    Pitch,
}

impl EffectKind {
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Pitch => "Pitch & formant",
        }
    }
}

// ---- serialisable settings ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PitchSettings {
    pub enabled: bool,
    pub mix: f32,
    /// Whole semitones, -12..12.
    pub semitones: f32,
    /// Fine tune in cents, -100..100.
    pub cents: f32,
    /// Formant shift in semitones, -12..12.
    pub formant: f32,
}

impl Default for PitchSettings {
    fn default() -> Self {
        Self { enabled: false, mix: 1.0, semitones: 0.0, cents: 0.0, formant: 0.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FxSettings {
    /// Processing order. Effects not listed are not in the chain.
    pub order: Vec<EffectKind>,
    pub pitch: PitchSettings,
}

impl Default for FxSettings {
    fn default() -> Self {
        Self { order: vec![EffectKind::Pitch], pitch: PitchSettings::default() }
    }
}

// ---- live (atomic) parameters ---------------------------------------------------------------

/// One parameter block per effect kind, shared with the processors via `Arc`.
#[derive(Debug, Default)]
pub struct FxParams {
    pub pitch: Arc<PitchParams>,
}

impl FxParams {
    pub fn from_settings(s: &FxSettings) -> Self {
        let p = Self::default();
        p.store(s);
        p
    }

    /// Publish settings to the audio thread (cheap: a handful of atomic stores).
    pub fn store(&self, s: &FxSettings) {
        let p = &self.pitch;
        p.slot.enabled.store(s.pitch.enabled, Relaxed);
        p.slot.mix.store(s.pitch.mix.clamp(0.0, 1.0));
        p.semitones.store(s.pitch.semitones + s.pitch.cents / 100.0);
        p.formant.store(s.pitch.formant);
    }

    fn slot_params(&self, kind: EffectKind) -> Arc<SlotParams> {
        match kind {
            EffectKind::Pitch => self.pitch.slot.clone(),
        }
    }

    fn make(&self, kind: EffectKind) -> Box<dyn Processor> {
        match kind {
            EffectKind::Pitch => Box::new(PitchShifter::new(self.pitch.clone())),
        }
    }
}

// ---- slots and chain ------------------------------------------------------------------------

pub struct Slot {
    kind: EffectKind,
    fx: Box<dyn Processor>,
    params: Arc<SlotParams>,
    enable: SmoothedValue,
    mix: SmoothedValue,
    dry_delay: DelayLine,
    input: Vec<f32>,
    is_reset: bool,
}

impl Slot {
    fn new(kind: EffectKind, fx_params: &FxParams, sample_rate: f32, max_block: usize) -> Self {
        let mut fx = fx_params.make(kind);
        fx.prepare(sample_rate, max_block);
        let params = fx_params.slot_params(kind);
        // Start at the current on/off state (a chain built while an effect is on starts on).
        let on = if params.enabled() { 1.0 } else { 0.0 };
        Self {
            kind,
            dry_delay: DelayLine::new(fx.latency()),
            fx,
            enable: SmoothedValue::new(on, sample_rate, FADE_SECONDS),
            mix: SmoothedValue::new(params.mix.load(), sample_rate, FADE_SECONDS),
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
        self.enable.set_target(if self.params.enabled() { 1.0 } else { 0.0 });
        self.mix.set_target(self.params.mix.load().clamp(0.0, 1.0));
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

    fn settings(enabled: bool, st: f32, mix: f32) -> FxSettings {
        FxSettings { pitch: PitchSettings { enabled, semitones: st, mix, ..Default::default() }, ..Default::default() }
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
        let fx = FxParams::from_settings(&settings(false, 7.0, 1.0));
        let mut chain = Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 0.2, 150.0);
        assert_eq!(run(&mut chain, &x), x);
        assert_eq!(chain.latency(), 0);
    }

    #[test]
    fn zero_mix_is_delayed_dry() {
        let fx = FxParams::from_settings(&settings(true, 7.0, 0.0));
        let mut chain = Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 0.2, 150.0);
        let y = run(&mut chain, &x);
        let d = chain.latency();
        assert_eq!(d, 3);
        assert_eq!(&y[d..], &x[..x.len() - d]);
    }

    #[test]
    fn toggling_does_not_click() {
        let fx = FxParams::from_settings(&settings(false, 5.0, 1.0));
        let mut chain = Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 1.2, 150.0);
        let mut y = x.clone();
        for (i, c) in y.chunks_mut(480).enumerate() {
            // On at 0.3 s, off at 0.8 s.
            fx.pitch.slot.set_enabled((30..80).contains(&i));
            chain.process(c);
        }
        // The input vowel's own largest step, with headroom for the crossfade.
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
}
