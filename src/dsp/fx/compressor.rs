//! Feed-forward compressor with a soft knee. Evens out loud and quiet speech.

use super::{coef, db_gain};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::Processor;
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Compressor",
    help: "Makes loud and quiet parts more even, so you're easy to hear without peaking.",
    params: &[
        ParamSpec { key: "threshold", label: "Threshold", min: -60.0, max: 0.0, default: -20.0, unit: " dB", step: 1.0, help: "Level above which compression starts." },
        ParamSpec { key: "ratio", label: "Ratio", min: 1.0, max: 20.0, default: 4.0, unit: ":1", step: 0.5, help: "4:1 = 4 dB louder input gives 1 dB louder output." },
        ParamSpec { key: "attack", label: "Attack", min: 0.1, max: 100.0, default: 5.0, unit: " ms", step: 0.1, help: "How fast it reacts to loud sounds." },
        ParamSpec { key: "release", label: "Release", min: 10.0, max: 1000.0, default: 120.0, unit: " ms", step: 1.0, help: "How fast it lets go afterwards." },
        ParamSpec { key: "makeup", label: "Makeup", min: 0.0, max: 24.0, default: 4.0, unit: " dB", step: 0.5, help: "Gain added after compression." },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    presets: &[
        ("Gentle", &[("threshold", -18.0), ("ratio", 2.0), ("makeup", 2.0)]),
        ("Broadcast", &[("threshold", -24.0), ("ratio", 4.0), ("attack", 3.0), ("makeup", 6.0)]),
        ("Squash", &[("threshold", -35.0), ("ratio", 12.0), ("attack", 1.0), ("makeup", 12.0)]),
    ],
};

const THRESHOLD: usize = 0;
const RATIO: usize = 1;
const ATTACK: usize = 2;
const RELEASE: usize = 3;
const MAKEUP: usize = 4;
const KNEE_DB: f32 = 6.0;

pub struct Compressor {
    params: Arc<EffectParams>,
    sr: f32,
    env: f32,
}

impl Compressor {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, sr: 48_000.0, env: 0.0 }
    }
}

/// Gain change in dB (<= 0) for an input level `over` dB above threshold.
#[inline]
fn gain_reduction(over: f32, slope: f32) -> f32 {
    if 2.0 * over < -KNEE_DB {
        0.0
    } else if 2.0 * over.abs() <= KNEE_DB {
        let t = over + KNEE_DB / 2.0;
        slope * t * t / (2.0 * KNEE_DB)
    } else {
        slope * over
    }
}

impl Processor for Compressor {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let thr = p.get(THRESHOLD);
        let slope = 1.0 / p.get(RATIO).max(1.0) - 1.0;
        let att = coef(p.get(ATTACK), self.sr);
        let rel = coef(p.get(RELEASE), self.sr);
        let makeup = p.get(MAKEUP);
        let mut max_gr = 0.0f32;
        for s in buf.iter_mut() {
            let a = s.abs();
            let c = if a > self.env { att } else { rel };
            self.env = a + (self.env - a) * c;
            let level = 20.0 * self.env.max(1e-6).log10();
            let gr = gain_reduction(level - thr, slope);
            max_gr = max_gr.min(gr);
            *s *= db_gain(gr + makeup);
        }
        p.meter.store(-max_gr);
    }

    fn reset(&mut self) {
        self.env = 0.0;
    }
}
