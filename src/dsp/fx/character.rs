//! Voice character: the qualities that make a voice sound like a particular person beyond its
//! pitch. Tone tilt, warmth, presence and nasality are filters; breathiness adds air that follows
//! your loudness; roughness adds an uneven, gravelly flutter; doubling layers a second, slightly
//! drifting copy of the voice. Every control at 0 passes audio through untouched.

use crate::dsp::Processor;
use crate::dsp::biquad::{Biquad, Shape};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::util::{Rng, SmoothedValue};
use std::f32::consts::TAU;
use std::sync::Arc;

const fn pct(key: &'static str, label: &'static str, min: f32, help: &'static str) -> ParamSpec {
    ParamSpec { key, label, min, max: 100.0, default: 0.0, unit: " %", step: 1.0, help }
}

pub const SPEC: EffectSpec = EffectSpec {
    label: "Voice character",
    help: "Changes the texture of your voice: dark or bright, warm, nasal, breathy, rough or doubled.",
    params: &[
        pct("tone", "Tone", -100.0, "- darker and duller, + brighter and thinner."),
        pct("warmth", "Warmth", 0.0, "Fullness in the low-mids (around 250 Hz): a bigger, cosier voice."),
        pct("presence", "Presence", -100.0, "Forwardness (around 3.5 kHz): + cuts through, - sounds distant."),
        pct("nasal", "Nasality", 0.0, "Talking-through-your-nose honk (around 1.1 kHz)."),
        pct("breath", "Breathiness", 0.0, "Adds airy breath that follows how loud you speak."),
        pct("rough", "Roughness", 0.0, "Uneven, gravelly flutter: older, tired or monstrous voices."),
        pct("double", "Doubling", 0.0, "Layers a second, slightly drifting copy: thicker, ghostly or two-voiced."),
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    choices: &[],
    random: &[
        ("tone", -50.0, 50.0),
        ("warmth", 0.0, 60.0),
        ("presence", -40.0, 40.0),
        ("nasal", 0.0, 40.0),
        ("breath", 0.0, 40.0),
        ("rough", 0.0, 30.0),
    ],
    presets: &[
        ("Radio host", &[("warmth", 60.0), ("presence", 40.0)]),
        ("Old", &[("tone", -30.0), ("breath", 30.0), ("rough", 40.0)]),
        ("Breathy", &[("breath", 70.0), ("tone", 20.0)]),
        ("Nasal", &[("nasal", 70.0)]),
        ("Gravelly", &[("rough", 60.0), ("warmth", 30.0)]),
        ("Double voice", &[("double", 70.0)]),
    ],
};

const TONE: usize = 0;
const WARMTH: usize = 1;
const PRESENCE: usize = 2;
const NASAL: usize = 3;
const BREATH: usize = 4;
const ROUGH: usize = 5;
const DOUBLE: usize = 6;

/// (shape, Hz, Q) of the four filters: tone low shelf, tone high shelf, warmth, presence, nasal.
const FILTERS: [(Shape, f32, f32); 5] = [
    (Shape::LowShelf, 300.0, 0.707),
    (Shape::HighShelf, 2500.0, 0.707),
    (Shape::Peak, 250.0, 0.8),
    (Shape::Peak, 3500.0, 1.0),
    (Shape::Peak, 1100.0, 2.0),
];
/// Same slewing as the equalizer: coefficients move at most 0.25 dB every 32 samples.
const UPDATE_EVERY: u64 = 32;
const MAX_STEP_DB: f32 = 0.25;

const ROUGH_HZ: f32 = 35.0;
const DOUBLE_MS: f32 = 18.0;
const DOUBLE_SWING_MS: f32 = 1.5;
const DOUBLE_LFO_HZ: f32 = 0.25;

pub struct Character {
    params: Arc<EffectParams>,
    sr: f32,
    n: u64,
    filters: [Biquad; 5],
    current_db: [f32; 5],
    breath: SmoothedValue,
    rough: SmoothedValue,
    double: SmoothedValue,
    // Breathiness: noise shaped to the breath band, scaled by the voice's envelope.
    env: f32,
    env_attack: f32,
    env_release: f32,
    noise_hp: Biquad,
    noise_lp: Biquad,
    rng: Rng,
    // Roughness: a random flutter, interpolated between random points ~35 times a second.
    flutter_from: f32,
    flutter_to: f32,
    flutter_pos: u32,
    flutter_len: u32,
    // Doubling: a modulated delay.
    delay: Vec<f32>,
    delay_mask: usize,
}

impl Character {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self {
            params,
            sr: 48_000.0,
            n: 0,
            filters: [Biquad::default(); 5],
            current_db: [0.0; 5],
            breath: SmoothedValue::new(0.0, 48_000.0, 0.02),
            rough: SmoothedValue::new(0.0, 48_000.0, 0.02),
            double: SmoothedValue::new(0.0, 48_000.0, 0.02),
            env: 0.0,
            env_attack: 0.0,
            env_release: 0.0,
            noise_hp: Biquad::default(),
            noise_lp: Biquad::default(),
            rng: Rng::new(0x5EED_C0DE),
            flutter_from: 0.0,
            flutter_to: 0.0,
            flutter_pos: 0,
            flutter_len: 1,
            delay: Vec::new(),
            delay_mask: 0,
        }
    }

    /// Filter gains in dB for the current settings, in `FILTERS` order.
    fn target_db(&self) -> [f32; 5] {
        let p = &self.params;
        let tilt = p.get(TONE) / 100.0 * 9.0;
        [
            -tilt / 2.0,
            tilt / 2.0,
            p.get(WARMTH) / 100.0 * 6.0,
            p.get(PRESENCE) / 100.0 * 6.0,
            p.get(NASAL) / 100.0 * 10.0,
        ]
    }

    fn update_filters(&mut self, target: &[f32; 5]) {
        for (i, (shape, f, q)) in FILTERS.iter().enumerate() {
            let d = (target[i] - self.current_db[i]).clamp(-MAX_STEP_DB, MAX_STEP_DB);
            if d != 0.0 {
                self.current_db[i] += d;
                self.filters[i].set(*shape, self.sr, *f, *q, self.current_db[i]);
            }
        }
    }

    #[inline]
    fn flutter(&mut self) -> f32 {
        if self.flutter_pos >= self.flutter_len {
            self.flutter_from = self.flutter_to;
            self.flutter_to = self.rng.next_f32();
            // Irregular segment lengths keep it from sounding like a steady tremolo.
            let jitter = 0.6 + 0.8 * self.rng.next_f32();
            self.flutter_len = ((self.sr / ROUGH_HZ * jitter) as u32).max(1);
            self.flutter_pos = 0;
        }
        let t = self.flutter_pos as f32 / self.flutter_len as f32;
        self.flutter_pos += 1;
        self.flutter_from + (self.flutter_to - self.flutter_from) * t
    }
}

fn active(v: &SmoothedValue) -> bool {
    !(v.is_settled() && v.current() == 0.0)
}

impl Processor for Character {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        let target = self.target_db();
        for (i, (shape, f, q)) in FILTERS.iter().enumerate() {
            self.current_db[i] = target[i];
            self.filters[i].set(*shape, sample_rate, *f, *q, target[i]);
        }
        let p = &self.params;
        for (v, i) in [(&mut self.breath, BREATH), (&mut self.rough, ROUGH), (&mut self.double, DOUBLE)] {
            *v = SmoothedValue::new(p.get(i) / 100.0, sample_rate, 0.02);
        }
        self.env_attack = super::coef(5.0, sample_rate);
        self.env_release = super::coef(80.0, sample_rate);
        self.noise_hp.set(Shape::HighPass, sample_rate, 1800.0, 0.707, 0.0);
        self.noise_lp.set(Shape::LowPass, sample_rate, (8000.0f32).min(sample_rate * 0.45), 0.707, 0.0);
        let len = ((DOUBLE_MS + DOUBLE_SWING_MS + 2.0) * 0.001 * sample_rate) as usize;
        self.delay = vec![0.0; len.next_power_of_two()];
        self.delay_mask = self.delay.len() - 1;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let target = self.target_db();
        let p = &self.params;
        self.breath.set_target(p.get(BREATH) / 100.0);
        self.rough.set_target(p.get(ROUGH) / 100.0);
        self.double.set_target(p.get(DOUBLE) / 100.0);
        let (breath_on, rough_on, double_on) = (active(&self.breath), active(&self.rough), active(&self.double));
        let sr = self.sr;

        for s in buf.iter_mut() {
            if self.n.is_multiple_of(UPDATE_EVERY) {
                self.update_filters(&target);
            }
            let n = self.n;
            self.n += 1;
            let dry = *s;
            let mut x = dry;
            for (f, g) in self.filters.iter_mut().zip(self.current_db) {
                if g != 0.0 {
                    x = f.process(x);
                }
            }

            // The envelope always runs so breath fades in at the right level when turned on.
            let a = dry.abs();
            let c = if a > self.env { self.env_attack } else { self.env_release };
            self.env = a + c * (self.env - a);

            if rough_on {
                let r = self.rough.next_value();
                let m = self.flutter();
                x *= 1.0 - 0.7 * r * m;
                // Gentle saturation (unity gain for small signals) adds grit.
                let drive = 1.0 + 4.0 * r;
                x = (drive * x).tanh() / drive;
            }

            if breath_on {
                let b = self.breath.next_value();
                let white = self.rng.next_f32() * 2.0 - 1.0;
                let air = self.noise_lp.process(self.noise_hp.process(white));
                x = x * (1.0 - 0.3 * b) + air * self.env * b * 1.5;
            }

            let w = n as usize & self.delay_mask;
            self.delay[w] = x;
            if double_on {
                let d = self.double.next_value();
                let lfo = (TAU * DOUBLE_LFO_HZ * (n as f64 / sr as f64) as f32).sin();
                let delay = (DOUBLE_MS + DOUBLE_SWING_MS * lfo) * 0.001 * sr;
                let pos = n as f32 - delay;
                let i = pos.floor();
                let frac = pos - i;
                let i = i as i64 as usize;
                let (a0, a1) = (self.delay[i & self.delay_mask], self.delay[(i + 1) & self.delay_mask]);
                let copy = a0 + (a1 - a0) * frac;
                let level = 0.8 * d;
                x = (x + level * copy) / (1.0 + 0.4 * level);
            }
            *s = x;
        }
    }

    fn reset(&mut self) {
        self.filters.iter_mut().for_each(Biquad::reset);
        self.noise_hp.reset();
        self.noise_lp.reset();
        self.env = 0.0;
        self.rng = Rng::new(0x5EED_C0DE);
        (self.flutter_from, self.flutter_to, self.flutter_pos, self.flutter_len) = (0.0, 0.0, 0, 1);
        self.delay.iter_mut().for_each(|v| *v = 0.0);
        self.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    fn run(x: &[f32], settings: &[(&str, f32)], block: usize) -> Vec<f32> {
        let params = Arc::new(EffectParams::new(&SPEC));
        for (k, v) in settings {
            params.set(SPEC.index(k).unwrap(), *v);
        }
        let mut fx = Character::new(params);
        fx.prepare(48_000.0, block);
        let mut y = x.to_vec();
        y.chunks_mut(block).for_each(|c| fx.process(c));
        y
    }

    /// Spectral energy of `x` in the band [lo, hi] Hz, summed over 4096-sample frames after the
    /// first 0.25 s.
    fn band(x: &[f32], lo: f32, hi: f32) -> f32 {
        let hz = 48_000.0 / 4096.0;
        x[12_000..]
            .as_chunks::<4096>()
            .0
            .iter()
            .flat_map(|c| {
                let s = crate::dsp::fft::spectrum(c);
                s.into_iter().enumerate().filter(|(k, _)| (lo..=hi).contains(&(*k as f32 * hz))).map(|(_, m)| m * m)
            })
            .sum()
    }

    #[test]
    fn neutral_is_bit_exact_passthrough() {
        let x = signals::vowel(48_000, 0.5, 140.0);
        assert_eq!(run(&x, &[], 480), x);
    }

    #[test]
    fn block_size_invariant() {
        let x = signals::vowel(48_000, 0.6, 140.0);
        let all = [
            ("tone", 30.0),
            ("warmth", 40.0),
            ("presence", -20.0),
            ("nasal", 50.0),
            ("breath", 50.0),
            ("rough", 50.0),
            ("double", 50.0),
        ];
        assert_eq!(run(&x, &all, 480), run(&x, &all, 37));
    }

    #[test]
    fn filters_move_the_right_bands() {
        let x = signals::vowel(48_000, 1.0, 140.0);
        let ratio = |s: &[(&str, f32)], lo, hi| band(&run(&x, s, 480), lo, hi) / band(&x, lo, hi);
        assert!(ratio(&[("tone", 100.0)], 4000.0, 8000.0) > 2.0, "bright");
        assert!(ratio(&[("tone", -100.0)], 4000.0, 8000.0) < 0.5, "dark");
        assert!(ratio(&[("nasal", 100.0)], 900.0, 1300.0) > 3.0, "nasal");
        assert!(ratio(&[("warmth", 100.0)], 180.0, 330.0) > 1.8, "warmth");
        assert!(ratio(&[("presence", -100.0)], 2800.0, 4400.0) < 0.6, "presence cut");
    }

    #[test]
    fn breath_adds_air_that_follows_the_voice() {
        // A low hum has nothing above 2 kHz: breath must add it, and only while the hum plays.
        let mut x = signals::sine(48_000, 1.0, 150.0, 0.3);
        x.extend(signals::silence(48_000, 1.0));
        let y = run(&x, &[("breath", 100.0)], 480);
        let (on, off) = (band(&y[..48_000], 2500.0, 7000.0), band(&y[48_000..], 2500.0, 7000.0));
        assert!(on > 1e-3, "air while speaking {on}");
        assert!(off < on * 1e-3, "air in silence {off} vs {on}");
    }

    #[test]
    fn roughness_makes_the_level_flutter() {
        let x = signals::sine(48_000, 1.0, 200.0, 0.3);
        let y = run(&x, &[("rough", 100.0)], 480);
        // RMS in 5 ms windows (one sine period) varies a lot with flutter, not without.
        let spread = |v: &[f32]| {
            let r: Vec<f32> = v[4800..].chunks(240).map(analysis::rms).collect();
            r.iter().cloned().fold(f32::MIN, f32::max) / r.iter().cloned().fold(f32::MAX, f32::min)
        };
        assert!(spread(&x) < 1.01);
        assert!(spread(&y) > 1.8, "{}", spread(&y));
    }

    #[test]
    fn doubling_adds_a_delayed_copy() {
        let mut x = vec![0.0f32; 4800];
        x[100] = 1.0;
        let y = run(&x, &[("double", 100.0)], 480);
        let at = 100 + (DOUBLE_MS * 48.0) as usize;
        let echo: f32 = y[at - 80..at + 80].iter().map(|v| v.abs()).sum();
        assert!(echo > 0.3, "{echo}");
        assert!(y[110..at - 80].iter().all(|v| *v == 0.0));
    }
}
