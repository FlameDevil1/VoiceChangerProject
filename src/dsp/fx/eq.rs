//! 5-band equaliser. Bands at 0 dB are skipped entirely, so a flat EQ is bit-exact and free.

use crate::dsp::Processor;
use crate::dsp::biquad::{Biquad, Shape};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use std::sync::Arc;

const fn band(key: &'static str, label: &'static str, help: &'static str) -> ParamSpec {
    ParamSpec { key, label, min: -12.0, max: 12.0, default: 0.0, unit: " dB", step: 0.5, help }
}

pub const SPEC: EffectSpec = EffectSpec {
    label: "Equalizer",
    help: "Shapes the tone of your voice: bass, body, clarity and air.",
    params: &[
        band("low", "Bass (100 Hz)", "Boom and depth."),
        band("low_mid", "Body (350 Hz)", "Warmth; cut to reduce muddiness."),
        band("mid", "Mid (1 kHz)", "Nasal / honky region."),
        band("high_mid", "Presence (3 kHz)", "Clarity and intelligibility."),
        band("high", "Air (8 kHz)", "Brightness and breath."),
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[],
    random: &[],
    presets: &[
        ("Warm", &[("low", 3.0), ("low_mid", 2.0), ("high", -2.0)]),
        ("Bright", &[("high_mid", 3.0), ("high", 4.0)]),
        ("Clear voice", &[("low", -4.0), ("low_mid", -2.0), ("high_mid", 3.0), ("high", 1.0)]),
        ("Bass boost", &[("low", 6.0), ("low_mid", 2.0)]),
    ],
};

const BANDS: [(Shape, f32, f32); 5] = [
    (Shape::LowShelf, 100.0, 0.707),
    (Shape::Peak, 350.0, 1.0),
    (Shape::Peak, 1000.0, 1.0),
    (Shape::Peak, 3000.0, 1.0),
    (Shape::HighShelf, 8000.0, 0.707),
];
/// Coefficients update every 32 samples (at fixed absolute positions), moving at most this many
/// dB per update: dragging a slider sweeps smoothly instead of zippering.
const UPDATE_EVERY: u64 = 32;
const MAX_STEP_DB: f32 = 0.25;

pub struct Eq {
    params: Arc<EffectParams>,
    sr: f32,
    filters: [Biquad; 5],
    current: [f32; 5],
    n: u64,
}

impl Eq {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, sr: 48_000.0, filters: [Biquad::default(); 5], current: [0.0; 5], n: 0 }
    }

    fn update(&mut self, targets: &[f32; 5]) {
        for (i, (shape, f, q)) in BANDS.iter().enumerate() {
            let d = (targets[i] - self.current[i]).clamp(-MAX_STEP_DB, MAX_STEP_DB);
            if d != 0.0 {
                self.current[i] += d;
                self.filters[i].set(*shape, self.sr, *f, *q, self.current[i]);
            }
        }
    }
}

impl Processor for Eq {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        // Start at the requested curve (no sweep from flat).
        let targets: [f32; 5] = std::array::from_fn(|i| self.params.get(i));
        for (i, (shape, f, q)) in BANDS.iter().enumerate() {
            self.current[i] = targets[i];
            self.filters[i].set(*shape, sample_rate, *f, *q, targets[i]);
        }
        self.n = 0;
    }

    fn process(&mut self, buf: &mut [f32]) {
        let targets: [f32; 5] = std::array::from_fn(|i| self.params.get(i));
        for s in buf.iter_mut() {
            if self.n.is_multiple_of(UPDATE_EVERY) {
                self.update(&targets);
            }
            self.n += 1;
            let mut x = *s;
            for (f, g) in self.filters.iter_mut().zip(self.current) {
                if g != 0.0 {
                    x = f.process(x);
                }
            }
            *s = x;
        }
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Biquad::reset);
    }
}
