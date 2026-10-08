//! Pitch & formant effect: drives the PSOLA engine from its parameters.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::pitch::{MAX_SHIFT_SEMITONES, PitchShifter, PsolaControls};
use std::sync::Arc;

const M: f32 = MAX_SHIFT_SEMITONES;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Pitch & formant",
    help: "Changes how high your voice is (pitch) and how big you sound (formant) independently.",
    params: &[
        ParamSpec {
            key: "semitones",
            label: "Pitch",
            min: -M,
            max: M,
            default: 0.0,
            unit: " st",
            step: 1.0,
            help: "Semitones. 12 = one octave.",
        },
        ParamSpec {
            key: "cents",
            label: "Fine tune",
            min: -100.0,
            max: 100.0,
            default: 0.0,
            unit: " ct",
            step: 1.0,
            help: "Hundredths of a semitone.",
        },
        ParamSpec {
            key: "formant",
            label: "Formant",
            min: -M,
            max: M,
            default: 0.0,
            unit: " st",
            step: 0.5,
            help: "Vocal tract size: + sounds smaller/younger, - sounds bigger/deeper.",
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[],
    random: &[],
    presets: &[
        ("Deeper", &[("semitones", -4.0), ("formant", -2.0)]),
        ("Higher", &[("semitones", 4.0), ("formant", 2.0)]),
        ("Male → female", &[("semitones", 6.0), ("formant", 3.0)]),
        ("Female → male", &[("semitones", -6.0), ("formant", -3.0)]),
        ("Child", &[("semitones", 8.0), ("formant", 5.0)]),
        ("Monster", &[("semitones", -12.0), ("formant", -6.0)]),
        ("Chipmunk", &[("semitones", 8.0), ("formant", 8.0)]),
    ],
};

const SEMITONES: usize = 0;
const CENTS: usize = 1;
const FORMANT: usize = 2;

pub struct PitchFx {
    params: Arc<EffectParams>,
    psola: PitchShifter,
}

impl PitchFx {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, psola: PitchShifter::new() }
    }
}

impl Processor for PitchFx {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.psola.prepare(sample_rate);
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let st = p.get(SEMITONES) + p.get(CENTS) / 100.0;
        self.psola.set_controls(PsolaControls::from_semitones(st, p.get(FORMANT)));
        self.psola.process(buf);
    }

    fn latency(&self) -> usize {
        self.psola.latency()
    }

    fn reset(&mut self) {
        self.psola.reset();
    }
}
