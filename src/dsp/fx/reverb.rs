//! Feedback-delay-network reverb: 8 delay lines mixed through an orthonormal Hadamard matrix.
//!
//! An FDN gives a dense, smooth tail at a fraction of the cost of convolution. Line lengths scale
//! with room size; each line's feedback gain is set from its length for the requested decay time
//! (RT60), and a one-pole lowpass per line makes highs die faster, like real rooms. Size changes
//! slew the line lengths slowly, so dragging the slider never clicks. Output is the reverb only;
//! the slot's mix control blends it with the dry voice.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Reverb",
    help: "Puts your voice in a space, from a small room to a cave.",
    params: &[
        ParamSpec {
            key: "size",
            label: "Room size",
            min: 0.0,
            max: 100.0,
            default: 50.0,
            unit: " %",
            step: 1.0,
            help: "Bigger rooms have later, sparser echoes.",
        },
        ParamSpec {
            key: "decay",
            label: "Decay",
            min: 0.1,
            max: 10.0,
            default: 1.5,
            unit: " s",
            step: 0.1,
            help: "Time for the tail to fade by 60 dB.",
        },
        ParamSpec {
            key: "damping",
            label: "Damping",
            min: 0.0,
            max: 100.0,
            default: 50.0,
            unit: " %",
            step: 1.0,
            help: "Soft, absorbent rooms lose treble quickly.",
        },
        ParamSpec {
            key: "predelay",
            label: "Pre-delay",
            min: 0.0,
            max: 100.0,
            default: 10.0,
            unit: " ms",
            step: 1.0,
            help: "Gap before the reverb starts; adds clarity.",
        },
    ],
    mix_label: "Mix",
    default_mix: 0.25,
    send_mix: true,
    choices: &[],
    random: &[],
    presets: &[
        ("Room", &[("size", 25.0), ("decay", 0.6), ("damping", 60.0), ("predelay", 5.0)]),
        ("Bathroom", &[("size", 15.0), ("decay", 1.2), ("damping", 10.0), ("predelay", 2.0)]),
        ("Hall", &[("size", 75.0), ("decay", 2.8), ("damping", 40.0), ("predelay", 25.0)]),
        ("Cave", &[("size", 100.0), ("decay", 6.0), ("damping", 25.0), ("predelay", 40.0)]),
    ],
};

const SIZE: usize = 0;
const DECAY: usize = 1;
const DAMPING: usize = 2;
const PREDELAY: usize = 3;

const N: usize = 8;
/// Mutually prime line lengths in samples at 48 kHz (21–50 ms).
const BASE: [f32; N] = [1031.0, 1259.0, 1453.0, 1621.0, 1823.0, 2003.0, 2203.0, 2399.0];
const SIGNS: [f32; N] = [1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0];
const MAX_SCALE: f32 = 1.5;
/// Max change of a line length per sample while the size slider moves (~2 % pitch wobble).
const SLEW: f32 = 0.02;

struct Line {
    buf: Vec<f32>,
    len: f32,
    lp: f32,
}

pub struct Reverb {
    params: Arc<EffectParams>,
    sr: f32,
    lines: Vec<Line>,
    mask: usize,
    pos: usize,
    pre: Vec<f32>,
    pre_mask: usize,
}

impl Reverb {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self { params, sr: 48_000.0, lines: Vec::new(), mask: 0, pos: 0, pre: Vec::new(), pre_mask: 0 }
    }

    fn target_len(&self, i: usize, size: f32) -> f32 {
        BASE[i] * (0.25 + 1.25 * size) * self.sr / 48_000.0
    }
}

/// In-place 8-point fast Walsh–Hadamard transform, scaled to be orthonormal (energy-preserving).
#[inline]
fn hadamard(v: &mut [f32; N]) {
    let mut h = 1;
    while h < N {
        for i in (0..N).step_by(h * 2) {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
        }
        h *= 2;
    }
    let s = 1.0 / (N as f32).sqrt();
    v.iter_mut().for_each(|x| *x *= s);
}

impl Processor for Reverb {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        let max_len = (BASE[N - 1] * MAX_SCALE * sample_rate / 48_000.0) as usize + 4;
        let size = max_len.next_power_of_two();
        self.mask = size - 1;
        let s = self.params.get(SIZE) / 100.0;
        self.lines = (0..N).map(|i| Line { buf: vec![0.0; size], len: self.target_len(i, s), lp: 0.0 }).collect();
        let pre = ((0.1 * sample_rate) as usize + 2).next_power_of_two();
        self.pre = vec![0.0; pre];
        self.pre_mask = pre - 1;
        self.pos = 0;
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let size = p.get(SIZE) / 100.0;
        let rt60 = p.get(DECAY).max(0.05);
        let damp = p.get(DAMPING) / 100.0 * 0.7;
        let pre_d = ((p.get(PREDELAY) * 0.001 * self.sr) as usize).min(self.pre_mask);
        let targets: [f32; N] = std::array::from_fn(|i| self.target_len(i, size));
        // Feedback gain per line for -60 dB after rt60 seconds, from its current length.
        let gains: [f32; N] = std::array::from_fn(|i| 10f32.powf(-3.0 * self.lines[i].len / (rt60 * self.sr)));
        let in_gain = 0.5 / (N as f32).sqrt();
        let out_gain = 1.0 / (N as f32).sqrt();

        for s in buf.iter_mut() {
            self.pre[self.pos & self.pre_mask] = *s;
            let x = self.pre[(self.pos.wrapping_sub(pre_d)) & self.pre_mask];

            let mut v = [0.0f32; N];
            let mut y = 0.0;
            for (i, line) in self.lines.iter_mut().enumerate() {
                line.len += (targets[i] - line.len).clamp(-SLEW, SLEW);
                let rp = self.pos as f32 - line.len;
                let i0 = rp.floor();
                let f = rp - i0;
                let i0 = i0 as i64 as usize;
                let a = line.buf[i0 & self.mask];
                let b = line.buf[i0.wrapping_add(1) & self.mask];
                let out = a + (b - a) * f;
                y += out * SIGNS[i];
                line.lp = out * (1.0 - damp) + line.lp * damp;
                v[i] = line.lp * gains[i];
            }
            hadamard(&mut v);
            for (i, line) in self.lines.iter_mut().enumerate() {
                line.buf[self.pos & self.mask] = v[i] + x * in_gain * SIGNS[i];
            }
            self.pos = self.pos.wrapping_add(1);
            *s = y * out_gain;
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.buf.fill(0.0);
            l.lp = 0.0;
        }
        self.pre.fill(0.0);
    }
}
