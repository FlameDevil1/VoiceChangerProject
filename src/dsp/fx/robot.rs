//! Robot voice: flattens your pitch to a fixed note (PSOLA monotone), then adds ring modulation
//! and a comb resonance tuned to that note for the metallic edge.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::pitch::{PitchShifter, PsolaControls};
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Robot",
    help: "Monotone, metallic robot voice.",
    params: &[
        ParamSpec {
            key: "pitch_hz",
            label: "Robot pitch",
            min: 50.0,
            max: 300.0,
            default: 110.0,
            unit: " Hz",
            step: 1.0,
            help: "The note the voice is flattened to.",
        },
        ParamSpec {
            key: "monotone",
            label: "Monotone",
            min: 0.0,
            max: 100.0,
            default: 100.0,
            unit: " %",
            step: 1.0,
            help: "100 % = perfectly flat; lower keeps some of your intonation.",
        },
        ParamSpec {
            key: "ring",
            label: "Ring mod",
            min: 0.0,
            max: 100.0,
            default: 25.0,
            unit: " %",
            step: 1.0,
            help: "Classic sci-fi warble.",
        },
        ParamSpec {
            key: "ring_hz",
            label: "Ring speed",
            min: 10.0,
            max: 200.0,
            default: 40.0,
            unit: " Hz",
            step: 1.0,
            help: "Frequency of the ring modulator.",
        },
        ParamSpec {
            key: "metallic",
            label: "Metallic",
            min: 0.0,
            max: 100.0,
            default: 35.0,
            unit: " %",
            step: 1.0,
            help: "Resonant, tinny edge.",
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[],
    random: &[],
    presets: &[
        ("Classic", &[("pitch_hz", 110.0), ("monotone", 100.0), ("ring", 25.0), ("metallic", 35.0)]),
        ("Dalek", &[("pitch_hz", 90.0), ("monotone", 70.0), ("ring", 80.0), ("ring_hz", 30.0), ("metallic", 20.0)]),
        ("Android", &[("pitch_hz", 160.0), ("monotone", 100.0), ("ring", 0.0), ("metallic", 60.0)]),
    ],
};

const PITCH_HZ: usize = 0;
const MONOTONE: usize = 1;
const RING: usize = 2;
const RING_HZ: usize = 3;
const METALLIC: usize = 4;

pub struct Robot {
    params: Arc<EffectParams>,
    sr: f32,
    psola: PitchShifter,
    phase: f64,
    comb: Vec<f32>,
    mask: usize,
    pos: usize,
}

impl Robot {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, sr: 48_000.0, psola: PitchShifter::new(), phase: 0.0, comb: Vec::new(), mask: 0, pos: 0 }
    }
}

impl Processor for Robot {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        self.psola.prepare(sample_rate);
        let size = ((sample_rate / 50.0) as usize + 2).next_power_of_two();
        self.comb = vec![0.0; size];
        self.mask = size - 1;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let hz = p.get(PITCH_HZ);
        self.psola.set_controls(PsolaControls {
            monotone: (p.get(MONOTONE) / 100.0) as f64,
            target_hz: hz as f64,
            ..Default::default()
        });
        self.psola.process(buf);

        let ring = p.get(RING) / 100.0;
        let inc = std::f64::consts::TAU * p.get(RING_HZ) as f64 / self.sr as f64;
        let g = 0.75 * p.get(METALLIC) / 100.0;
        let d = ((self.sr / hz).round() as usize).clamp(1, self.mask);
        for s in buf.iter_mut() {
            let mut y = *s;
            if ring > 0.0 {
                y *= 1.0 - ring + ring * self.phase.sin() as f32;
            }
            self.phase = (self.phase + inc) % std::f64::consts::TAU;
            let c = y + g * self.comb[self.pos.wrapping_sub(d) & self.mask];
            self.comb[self.pos & self.mask] = c;
            self.pos = self.pos.wrapping_add(1);
            *s = c * (1.0 - 0.6 * g);
        }
    }

    fn latency(&self) -> usize {
        self.psola.latency()
    }

    fn reset(&mut self) {
        self.psola.reset();
        self.comb.fill(0.0);
        self.phase = 0.0;
    }
}
