//! Radio / telephone: steep band-pass, saturation and optional static.

use crate::dsp::Processor;
use crate::dsp::biquad::{Biquad, Shape};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::util::Rng;
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Radio / telephone",
    help: "Thin, band-limited sound of a phone line, radio or walkie-talkie.",
    params: &[
        ParamSpec {
            key: "low_cut",
            label: "Low cut",
            min: 100.0,
            max: 1500.0,
            default: 300.0,
            unit: " Hz",
            step: 10.0,
            help: "Removes bass below this.",
        },
        ParamSpec {
            key: "high_cut",
            label: "High cut",
            min: 1500.0,
            max: 8000.0,
            default: 3400.0,
            unit: " Hz",
            step: 50.0,
            help: "Removes treble above this.",
        },
        ParamSpec {
            key: "drive",
            label: "Distortion",
            min: 0.0,
            max: 100.0,
            default: 30.0,
            unit: " %",
            step: 1.0,
            help: "Crunchy overdriven speaker.",
        },
        ParamSpec {
            key: "noise",
            label: "Static",
            min: 0.0,
            max: 100.0,
            default: 0.0,
            unit: " %",
            step: 1.0,
            help: "Background hiss.",
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    presets: &[
        ("Telephone", &[("low_cut", 300.0), ("high_cut", 3400.0), ("drive", 20.0), ("noise", 0.0)]),
        ("AM radio", &[("low_cut", 450.0), ("high_cut", 4500.0), ("drive", 40.0), ("noise", 20.0)]),
        ("Walkie-talkie", &[("low_cut", 700.0), ("high_cut", 2800.0), ("drive", 70.0), ("noise", 30.0)]),
    ],
};

const LOW_CUT: usize = 0;
const HIGH_CUT: usize = 1;
const DRIVE: usize = 2;
const NOISE: usize = 3;
const UPDATE_EVERY: u64 = 32;
/// Max cutoff change per update, as a frequency ratio (smooth sweeps, no zipper).
const MAX_RATIO: f32 = 1.03;

pub struct Radio {
    params: Arc<EffectParams>,
    sr: f32,
    hp: [Biquad; 2],
    lp: [Biquad; 2],
    low: f32,
    high: f32,
    rng: Rng,
    n: u64,
}

impl Radio {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self {
            params,
            sr: 48_000.0,
            hp: [Biquad::default(); 2],
            lp: [Biquad::default(); 2],
            low: 300.0,
            high: 3400.0,
            rng: Rng::new(0xC0FFEE),
            n: 0,
        }
    }

    fn set_filters(&mut self) {
        // Two cascaded Butterworth sections per edge = 24 dB/octave, the "boxed in" phone sound.
        for f in &mut self.hp {
            f.set(Shape::HighPass, self.sr, self.low, 0.707, 0.0);
        }
        for f in &mut self.lp {
            f.set(Shape::LowPass, self.sr, self.high, 0.707, 0.0);
        }
    }
}

fn slew(cur: f32, target: f32) -> f32 {
    target.clamp(cur / MAX_RATIO, cur * MAX_RATIO)
}

impl Processor for Radio {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        self.low = self.params.get(LOW_CUT);
        self.high = self.params.get(HIGH_CUT);
        self.set_filters();
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let (low_t, high_t) = (p.get(LOW_CUT), p.get(HIGH_CUT));
        let k = 1.0 + 9.0 * p.get(DRIVE) / 100.0;
        // Normalised so quiet speech keeps roughly its level while peaks are squashed.
        let drive_gain = k.sqrt() / k;
        let noise = 0.03 * (p.get(NOISE) / 100.0).powi(2);
        for s in buf.iter_mut() {
            if self.n.is_multiple_of(UPDATE_EVERY) && (self.low != low_t || self.high != high_t) {
                self.low = slew(self.low, low_t);
                self.high = slew(self.high, high_t);
                self.set_filters();
            }
            self.n += 1;
            let mut x = *s;
            if noise > 0.0 {
                x += noise * (2.0 * self.rng.next_f32() - 1.0);
            }
            for f in &mut self.hp {
                x = f.process(x);
            }
            if k > 1.0 {
                x = (k * x).tanh() * drive_gain;
            }
            for f in &mut self.lp {
                x = f.process(x);
            }
            *s = x;
        }
    }

    fn reset(&mut self) {
        self.hp.iter_mut().chain(self.lp.iter_mut()).for_each(Biquad::reset);
    }
}
