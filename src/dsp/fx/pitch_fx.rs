//! Pitch & formant effect: drives the PSOLA engine from its parameters.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::pitch::{MAX_SHIFT_SEMITONES, PitchShifter, PsolaControls, Scale};
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
        ParamSpec {
            key: "intonation",
            label: "Intonation",
            min: 0.0,
            max: 200.0,
            default: 100.0,
            unit: " %",
            step: 5.0,
            help: "How much your pitch moves as you talk. 0 = flat and robotic, 100 = natural, 200 = sing-song.",
        },
        ParamSpec {
            key: "vibrato",
            label: "Vibrato",
            min: 0.0,
            max: 100.0,
            default: 0.0,
            unit: " ct",
            step: 1.0,
            help: "Pitch wobble depth in cents. 20-40 is a gentle wavering voice, 100 is a warble.",
        },
        ParamSpec {
            key: "vibrato_rate",
            label: "Vibrato speed",
            min: 1.0,
            max: 10.0,
            default: 5.5,
            unit: " Hz",
            step: 0.1,
            help: "Wobbles per second. Singers use about 5-6.",
        },
        ParamSpec {
            key: "autotune",
            label: "Auto-tune",
            min: 0.0,
            max: 100.0,
            default: 0.0,
            unit: " %",
            step: 1.0,
            help: "Pulls your pitch to the nearest note. Around 50 % is subtle correction, 100 % is the hard robotic effect.",
        },
        ParamSpec {
            key: "key",
            label: "Key",
            min: 0.0,
            max: 11.0,
            default: 0.0,
            unit: "",
            step: 1.0,
            help: "Root note of the auto-tune scale.",
        },
        ParamSpec {
            key: "scale",
            label: "Scale",
            min: 0.0,
            max: 3.0,
            default: 0.0,
            unit: "",
            step: 1.0,
            help: "Notes auto-tune may use. Chromatic = every note (works with any song).",
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[("key", &NOTE_NAMES), ("scale", Scale::LABELS)],
    random: &[("semitones", -6.0, 6.0), ("formant", -4.0, 4.0), ("intonation", 60.0, 140.0)],
    presets: &[
        ("Deeper", &[("semitones", -4.0), ("formant", -2.0)]),
        ("Higher", &[("semitones", 4.0), ("formant", 2.0)]),
        ("Male to female", &[("semitones", 6.0), ("formant", 3.0)]),
        ("Female to male", &[("semitones", -6.0), ("formant", -3.0)]),
        ("Child", &[("semitones", 8.0), ("formant", 5.0)]),
        ("Monster", &[("semitones", -12.0), ("formant", -6.0)]),
        ("Chipmunk", &[("semitones", 8.0), ("formant", 8.0)]),
        ("Hard auto-tune", &[("autotune", 100.0)]),
        ("Wavering", &[("vibrato", 35.0), ("vibrato_rate", 6.0)]),
    ],
};

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

const SEMITONES: usize = 0;
const CENTS: usize = 1;
const FORMANT: usize = 2;
const INTONATION: usize = 3;
const VIBRATO: usize = 4;
const VIBRATO_RATE: usize = 5;
const AUTOTUNE: usize = 6;
const KEY: usize = 7;
const SCALE: usize = 8;

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
        self.psola.set_controls(PsolaControls {
            intonation: p.get(INTONATION) as f64 / 100.0,
            vibrato_cents: p.get(VIBRATO) as f64,
            vibrato_hz: p.get(VIBRATO_RATE) as f64,
            autotune: p.get(AUTOTUNE) as f64 / 100.0,
            key: p.get(KEY).round() as i32,
            scale: Scale::from_index(p.get(SCALE)),
            ..PsolaControls::from_semitones(st, p.get(FORMANT))
        });
        self.psola.process(buf);
    }

    fn latency(&self) -> usize {
        self.psola.latency()
    }

    fn reset(&mut self) {
        self.psola.reset();
    }
}
