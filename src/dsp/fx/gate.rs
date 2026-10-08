//! Noise gate with hysteresis and hold, so it doesn't chatter or chop word endings.

use super::{coef, db_gain};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::{Processor, db_to_gain};
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Noise gate",
    help: "Turns the mic down between words. Set the threshold just above your background noise.",
    params: &[
        ParamSpec {
            key: "threshold",
            label: "Threshold",
            min: -80.0,
            max: 0.0,
            default: -45.0,
            unit: " dB",
            step: 1.0,
            help: "Sound quieter than this is reduced.",
        },
        ParamSpec {
            key: "reduction",
            label: "Reduction",
            min: 0.0,
            max: 80.0,
            default: 40.0,
            unit: " dB",
            step: 1.0,
            help: "How much quieter it gets when closed (80 = silent).",
        },
        ParamSpec {
            key: "attack",
            label: "Attack",
            min: 0.1,
            max: 50.0,
            default: 1.0,
            unit: " ms",
            step: 0.1,
            help: "Time to open fully when you start talking.",
        },
        ParamSpec {
            key: "hold",
            label: "Hold",
            min: 0.0,
            max: 500.0,
            default: 80.0,
            unit: " ms",
            step: 1.0,
            help: "How long it stays open after you stop.",
        },
        ParamSpec {
            key: "release",
            label: "Release",
            min: 5.0,
            max: 1000.0,
            default: 150.0,
            unit: " ms",
            step: 1.0,
            help: "Time to fade out fully after the hold.",
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[],
    random: &[],
    presets: &[],
};

const THRESHOLD: usize = 0;
const REDUCTION: usize = 1;
const ATTACK: usize = 2;
const HOLD: usize = 3;
const RELEASE: usize = 4;
/// The gate closes this far below the opening threshold, so it doesn't flutter at the edge.
const HYSTERESIS_DB: f32 = 4.0;

pub struct Gate {
    params: Arc<EffectParams>,
    sr: f32,
    env: f32,
    env_decay: f32,
    /// Current gain in dB (0 = open, -reduction = closed) and as a linear factor.
    gain_db: f32,
    gain: f32,
    hold_left: u32,
    open: bool,
}

impl Gate {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, sr: 48_000.0, env: 0.0, env_decay: 0.0, gain_db: 0.0, gain: 1.0, hold_left: 0, open: true }
    }
}

impl Processor for Gate {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        // Peak detector releases over 10 ms: smooths ripple within a pitch period.
        self.env_decay = coef(10.0, sample_rate);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let thr = p.get(THRESHOLD);
        let open_at = db_to_gain(thr);
        let close_at = db_to_gain(thr - HYSTERESIS_DB);
        let range = p.get(REDUCTION);
        // Fades are linear in dB: the gate crosses its whole range in exactly attack/release ms.
        // (At least 1 dB of span, so a gate left closed still reopens if Reduction is set to 0.)
        let span = range.max(1.0);
        let up = span / (p.get(ATTACK) * 0.001 * self.sr).max(1.0);
        let down = span / (p.get(RELEASE) * 0.001 * self.sr).max(1.0);
        let hold = (p.get(HOLD) * 0.001 * self.sr) as u32;

        for s in buf.iter_mut() {
            let a = s.abs();
            self.env = if a > self.env { a } else { self.env * self.env_decay };
            if self.env >= open_at {
                self.open = true;
                self.hold_left = hold;
            } else if self.open && self.env >= close_at {
                self.hold_left = hold;
            } else if self.open {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            let target = if self.open { 0.0 } else { -range };
            if self.gain_db != target {
                self.gain_db += (target - self.gain_db).clamp(-down, up);
                self.gain = db_gain(self.gain_db);
            }
            *s *= self.gain;
        }
        p.meter.store(if self.open { 1.0 } else { 0.0 });
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.gain_db = 0.0;
        self.gain = 1.0;
        self.hold_left = 0;
        self.open = true;
    }
}
