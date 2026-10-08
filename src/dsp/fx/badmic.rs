//! Bad mic: cheap and faulty microphone problems. Room boxiness, plosive pops, handling thumps
//! and wind reach the capsule; the capsule's narrow bandwidth shapes them with the voice; then
//! the electronics add hiss, mains hum and crackle; a cheap headset's auto-gain pumps, the level
//! drifts, the converter clips; finally an over-eager gate chops words and a loose cable drops
//! out. Nothing here adds delay, and every control at 0 passes audio through untouched.

use crate::dsp::Processor;
use crate::dsp::biquad::{Biquad, Shape};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::util::Rng;
use std::f32::consts::TAU;
use std::sync::Arc;

const fn pct(key: &'static str, label: &'static str, help: &'static str) -> ParamSpec {
    ParamSpec { key, label, min: 0.0, max: 100.0, default: 0.0, unit: " %", step: 1.0, help }
}

pub const SPEC: EffectSpec = EffectSpec {
    label: "Bad mic",
    help: "Cheap or faulty microphone: thin sound, distortion, hiss, hum, crackle, bumps, wind, pumping and cut-outs. Turn up the problems you want.",
    params: &[
        ParamSpec {
            key: "low_cut",
            label: "Low cut",
            min: 20.0,
            max: 1000.0,
            default: 20.0,
            unit: " Hz",
            step: 10.0,
            help: "Removes bass: higher = thinner, tinnier. 20 = off.",
        },
        ParamSpec {
            key: "high_cut",
            label: "High cut",
            min: 1000.0,
            max: 20000.0,
            default: 20000.0,
            unit: " Hz",
            step: 100.0,
            help: "Removes treble: lower = duller, muffled. 20000 = off.",
        },
        pct("clip", "Clipping", "Harsh distortion when you speak loudly, like a mic set too hot."),
        pct("hiss", "Hiss", "Constant background noise floor."),
        pct("hum", "Hum", "Electrical buzz from mains power and its harmonics."),
        ParamSpec {
            key: "hum_hz",
            label: "Mains",
            min: 0.0,
            max: 1.0,
            default: 1.0,
            unit: "",
            step: 1.0,
            help: "Mains frequency of the hum: 50 Hz (Europe, most of Asia/Africa) or 60 Hz (Americas).",
        },
        pct("crackle", "Crackle", "Random crackles and static, like a loose connection."),
        pct("pops", "Plosive pops", "Exaggerated thumps on \"p\" and \"b\" sounds."),
        pct("room", "Boxy room", "Close reflections of a small, untreated room: hollow and boxy."),
        pct("handling", "Handling bumps", "Random thumps, as if the mic is being touched or moved."),
        pct("wind", "Wind", "Low rumbling gusts, like talking outside."),
        pct("drift", "Level drift", "The volume slowly wanders up and down."),
        pct("pump", "Auto-gain", "Cheap headset gain control: background noise swells whenever you stop talking."),
        pct("gate", "Cutting in/out", "Over-eager noise gate that clips the starts and ends of words."),
        pct("dropout", "Dropouts", "Brief total silences, like a loose cable."),
        ParamSpec {
            key: "variation",
            default: 50.0,
            ..pct(
                "variation",
                "Variation",
                "How much the problems come and go: 0 = steady, 100 = quiet stretches and bad patches.",
            )
        },
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    send_mix: false,
    choices: &[("hum_hz", &["50 Hz", "60 Hz"])],
    random: &[
        ("low_cut", 20.0, 500.0),
        ("high_cut", 3000.0, 20000.0),
        ("clip", 0.0, 50.0),
        ("hiss", 0.0, 60.0),
        ("hum", 0.0, 40.0),
        ("crackle", 0.0, 40.0),
        ("room", 0.0, 60.0),
        ("pump", 0.0, 60.0),
    ],
    presets: &[
        ("Cheap headset", &[("low_cut", 250.0), ("high_cut", 5000.0), ("hiss", 35.0), ("clip", 20.0), ("pump", 50.0)]),
        (
            "Old webcam",
            &[
                ("low_cut", 200.0),
                ("high_cut", 6000.0),
                ("hiss", 55.0),
                ("room", 45.0),
                ("pump", 70.0),
                ("drift", 20.0),
            ],
        ),
        (
            "Loose cable",
            &[("crackle", 70.0), ("dropout", 50.0), ("hum", 40.0), ("handling", 20.0), ("variation", 70.0)],
        ),
        ("Windy outside", &[("wind", 70.0), ("pops", 40.0), ("handling", 15.0)]),
        ("Too hot", &[("clip", 80.0), ("pops", 50.0)]),
    ],
};

const LOW_CUT: usize = 0;
const HIGH_CUT: usize = 1;
const CLIP: usize = 2;
const HISS: usize = 3;
const HUM: usize = 4;
const HUM_HZ: usize = 5;
const CRACKLE: usize = 6;
const POPS: usize = 7;
const ROOM: usize = 8;
const HANDLING: usize = 9;
const WIND: usize = 10;
const DRIFT: usize = 11;
const PUMP: usize = 12;
const GATE: usize = 13;
const DROPOUT: usize = 14;
const VARIATION: usize = 15;

/// Filter settings move at most this much every 32 samples (no zipper noise while dragging).
const UPDATE_EVERY: u32 = 32;
const MAX_STEP_DB: f32 = 0.25;
const MAX_STEP_RATIO: f32 = 1.03;
/// Early reflections of a small room: (ms, gain at 100 %).
const TAPS: [(f32, f32); 5] = [(2.9, 0.7), (4.7, 0.55), (7.3, 0.45), (11.1, 0.35), (13.7, 0.3)];
const ROOM_COMB_MS: f32 = 9.3;
const HUM_TABLE: usize = 1024;

/// Exponential decay per sample for a time constant in ms.
fn decay(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms * 0.001 * sr)).exp()
}

/// Duration in samples (at least 1).
fn ms(sr: f32, ms: f32) -> u32 {
    (ms * 0.001 * sr).max(1.0) as u32
}

pub struct BadMic {
    params: Arc<EffectParams>,
    sr: f32,
    rng: Rng,
    update_left: u32,
    tick_left: u32,
    tick_len: u32,
    weather: f32,
    weather_target: f32,
    weather_left: u32,
    // Bandwidth (two 2nd-order sections each way) and the room's boxy resonance.
    hp: [Biquad; 2],
    lp: [Biquad; 2],
    low_hz: f32,
    high_hz: f32,
    room_peak: Biquad,
    room_db: f32,
    room_buf: Vec<f32>,
    room_mask: usize,
    room_pos: usize,
    room_taps: [usize; 5],
    room_comb: usize,
    // Plosives: low band, fast and slow envelopes of it.
    pop_lp: Biquad,
    pop_fast: f32,
    pop_slow: f32,
    // Handling thump: decaying low sine plus a rustle.
    thump_amp: f32,
    thump_phase: f32,
    thump_inc: f32,
    thump_decay: f32,
    rustle_lp: Biquad,
    // Wind: low-passed noise times a gust envelope.
    wind_lp: [Biquad; 2],
    gust: f32,
    gust_target: f32,
    gust_left: u32,
    // Pink hiss (Paul Kellet's economy filter).
    pink: [f32; 3],
    // Hum: one cycle of a buzzy waveform, read at the mains frequency.
    hum_table: Vec<f32>,
    hum_phase: f32,
    // Crackle: decaying clicks, clustered into bursts.
    click: f32,
    click_decay: f32,
    crackle_on: bool,
    crackle_left: u32,
    // Auto-gain and level drift.
    agc_env: f32,
    agc_gain: f32,
    drift_db: f32,
    drift_target: f32,
    drift_left: u32,
    // Gate and dropouts.
    gate_env: f32,
    gate_peak: f32,
    gate_gain: f32,
    drop_left: u32,
    drop_gain: f32,
}

impl BadMic {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self {
            params,
            sr: 48_000.0,
            rng: Rng::new(0x0BAD_311C),
            update_left: 0,
            tick_left: 0,
            tick_len: 480,
            weather: 1.0,
            weather_target: 1.0,
            weather_left: 0,
            hp: [Biquad::default(); 2],
            lp: [Biquad::default(); 2],
            low_hz: 20.0,
            high_hz: 20_000.0,
            room_peak: Biquad::default(),
            room_db: 0.0,
            room_buf: Vec::new(),
            room_mask: 0,
            room_pos: 0,
            room_taps: [0; 5],
            room_comb: 1,
            pop_lp: Biquad::default(),
            pop_fast: 0.0,
            pop_slow: 0.0,
            thump_amp: 0.0,
            thump_phase: 0.0,
            thump_inc: 0.0,
            thump_decay: 0.0,
            rustle_lp: Biquad::default(),
            wind_lp: [Biquad::default(); 2],
            gust: 0.0,
            gust_target: 0.0,
            gust_left: 0,
            pink: [0.0; 3],
            hum_table: Vec::new(),
            hum_phase: 0.0,
            click: 0.0,
            click_decay: 0.0,
            crackle_on: false,
            crackle_left: 0,
            agc_env: 0.0,
            agc_gain: 1.0,
            drift_db: 0.0,
            drift_target: 0.0,
            drift_left: 0,
            gate_env: 0.0,
            gate_peak: 0.0,
            gate_gain: 1.0,
            drop_left: 0,
            drop_gain: 1.0,
        }
    }

    fn uniform(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rng.next_f32()
    }

    /// Off values of the bandwidth controls (the high cut is also off above ~Nyquist).
    fn targets(&self) -> (f32, f32, f32) {
        let p = &self.params;
        let low = p.get(LOW_CUT);
        let high = p.get(HIGH_CUT).min(self.sr * 0.45);
        (low, high, p.get(ROOM) / 100.0 * 6.0)
    }

    fn high_off(&self, hz: f32) -> bool {
        hz >= 19_999.0 || hz >= self.sr * 0.45
    }

    /// Move the filters one step towards their targets.
    fn update_filters(&mut self, (low, high, room_db): (f32, f32, f32)) {
        let step = |cur: f32, target: f32| target.clamp(cur / MAX_STEP_RATIO, cur * MAX_STEP_RATIO);
        if self.low_hz != low {
            self.low_hz = step(self.low_hz, low);
            for f in &mut self.hp {
                f.set(Shape::HighPass, self.sr, self.low_hz, 0.707, 0.0);
            }
        }
        if self.high_hz != high {
            self.high_hz = step(self.high_hz, high);
            for f in &mut self.lp {
                f.set(Shape::LowPass, self.sr, self.high_hz, 0.707, 0.0);
            }
        }
        if self.room_db != room_db {
            self.room_db += (room_db - self.room_db).clamp(-MAX_STEP_DB, MAX_STEP_DB);
            self.room_peak.set(Shape::Peak, self.sr, 450.0, 1.2, self.room_db);
        }
    }

    /// Every 10 ms: drift the "weather" (how bad things are right now).
    fn on_tick(&mut self, variation: f32) {
        if self.weather_left == 0 {
            self.weather_target = 1.0 + variation * (2.0 * self.rng.next_f32() - 1.0);
            self.weather_left = 200 + (self.rng.next_f32() * 600.0) as u32;
        }
        self.weather_left -= 1;
        self.weather += (self.weather_target - self.weather) * 0.01;
    }

    /// Bernoulli draw for an event with `per_second` average rate, this sample.
    fn chance(&mut self, per_second: f32) -> bool {
        per_second > 0.0 && self.rng.next_f32() < per_second * self.weather.max(0.0) / self.sr
    }
}

impl Processor for BadMic {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        self.tick_len = ms(self.sr, 10.0);
        let room_len = ((TAPS[4].0.max(ROOM_COMB_MS) + 2.0) * 0.001 * sample_rate) as usize;
        self.room_buf = vec![0.0; room_len.next_power_of_two()];
        self.room_mask = self.room_buf.len() - 1;
        self.room_taps = TAPS.map(|(ms, _)| (ms * 0.001 * sample_rate) as usize);
        self.room_comb = ((ROOM_COMB_MS * 0.001 * sample_rate) as usize).max(1);
        self.pop_lp.set(Shape::LowPass, sample_rate, 150.0, 0.707, 0.0);
        self.rustle_lp.set(Shape::LowPass, sample_rate, 800.0, 0.707, 0.0);
        for f in &mut self.wind_lp {
            f.set(Shape::LowPass, sample_rate, 120.0, 0.707, 0.0);
        }
        // A buzzy mains waveform: odd harmonics dominate, normalised to peak 1.
        let mut table: Vec<f32> = (0..=HUM_TABLE)
            .map(|i| {
                let ph = TAU * i as f32 / HUM_TABLE as f32;
                (1..=7).map(|k| (k as f32 * ph).sin() / (k as f32).powf(if k % 2 == 1 { 0.6 } else { 1.2 })).sum()
            })
            .collect();
        let peak = table.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        table.iter_mut().for_each(|v| *v /= peak);
        self.hum_table = table;
        // Start at the requested settings (no sweep from "off").
        let (low, high, room_db) = self.targets();
        (self.low_hz, self.high_hz, self.room_db) = (low, high, room_db);
        for f in &mut self.hp {
            f.set(Shape::HighPass, sample_rate, low, 0.707, 0.0);
        }
        for f in &mut self.lp {
            f.set(Shape::LowPass, sample_rate, high, 0.707, 0.0);
        }
        self.room_peak.set(Shape::Peak, sample_rate, 450.0, 1.2, room_db);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let v: [f32; 16] = std::array::from_fn(|i| if i < 2 || i == HUM_HZ { p.get(i) } else { p.get(i) / 100.0 });
        let targets = self.targets();
        let sr = self.sr;
        let hum_hz = if v[HUM_HZ] >= 0.5 { 60.0 } else { 50.0 };
        let hum_level = if v[HUM] > 0.0 { 10f32.powf((-62.0 + 40.0 * v[HUM]) / 20.0) } else { 0.0 };
        let hiss_level = if v[HISS] > 0.0 { 10f32.powf((-72.0 + 42.0 * v[HISS]) / 20.0) } else { 0.0 };
        let clip_t = 10f32.powf(-24.0 * v[CLIP] / 20.0);
        // Partial makeup: loud speech gets crunched without the whole voice getting louder.
        let clip_makeup = 10f32.powf(5.0 * v[CLIP] / 20.0);
        let room = v[ROOM];
        let (attack, release) = (decay(0.5, sr), decay(30.0, sr));
        let gate_thr_db = -40.0 + 34.0 * v[GATE];
        let gate_open = 1.0 / (0.002 + 0.013 * v[GATE]) / sr;
        let gate_close = 1.0 / 0.005 / sr;
        let peak_release = decay(3000.0, sr);
        let agc_env_c = decay(50.0, sr);
        let (agc_down, agc_up) = (decay(30.0, sr), decay(800.0, sr));
        let agc_max = 10f32.powf(24.0 * v[PUMP] / 20.0);
        let drift_c = decay(300.0, sr);
        let pop_fast = (decay(1.0, sr), decay(20.0, sr));
        let pop_slow = (decay(40.0, sr), decay(200.0, sr));

        for s in buf.iter_mut() {
            if self.update_left == 0 {
                self.update_left = UPDATE_EVERY;
                self.update_filters(targets);
            }
            self.update_left -= 1;
            if self.tick_left == 0 {
                self.tick_left = self.tick_len;
                self.on_tick(v[VARIATION]);
            }
            self.tick_left -= 1;
            let mut y = *s;

            // --- acoustics: what reaches the capsule ---
            if room > 0.0 || self.room_db != 0.0 {
                let at = |d: usize| self.room_buf[self.room_pos.wrapping_sub(d) & self.room_mask];
                let reflections: f32 = TAPS.iter().zip(self.room_taps).map(|((_, g), d)| g * at(d)).sum();
                let comb = at(self.room_comb);
                let w = y + room * (reflections * 0.6 + 0.45 * comb);
                self.room_buf[self.room_pos & self.room_mask] = y + 0.45 * room * comb;
                self.room_pos = self.room_pos.wrapping_add(1);
                y = self.room_peak.process(w / (1.0 + 0.9 * room));
            }
            if v[POPS] > 0.0 {
                let low = self.pop_lp.process(y);
                let a = low.abs();
                let c = if a > self.pop_fast { pop_fast.0 } else { pop_fast.1 };
                self.pop_fast = a + c * (self.pop_fast - a);
                let c = if a > self.pop_slow { pop_slow.0 } else { pop_slow.1 };
                self.pop_slow = a + c * (self.pop_slow - a);
                let burst = ((self.pop_fast - 1.5 * self.pop_slow) / (self.pop_fast + 1e-6)).clamp(0.0, 1.0);
                y += low * 6.0 * v[POPS] * burst;
            }
            if v[HANDLING] > 0.0 {
                if self.chance(0.4 * v[HANDLING]) {
                    self.thump_amp = self.uniform(0.15, 0.5) * v[HANDLING];
                    self.thump_inc = TAU * self.uniform(25.0, 70.0) / sr;
                    self.thump_decay = decay(self.uniform(40.0, 120.0), sr);
                    self.thump_phase = 0.0;
                }
                if self.thump_amp > 1e-5 {
                    let rustle = self.rustle_lp.process(self.rng.next_f32() * 2.0 - 1.0);
                    y += self.thump_amp * (self.thump_phase.sin() + 0.3 * rustle);
                    self.thump_phase = (self.thump_phase + self.thump_inc) % TAU;
                    self.thump_amp *= self.thump_decay;
                }
            }
            if v[WIND] > 0.0 {
                if self.gust_left == 0 {
                    let g = self.rng.next_f32();
                    self.gust_target = g * g * self.weather.clamp(0.0, 2.0);
                    self.gust_left = ms(self.sr, self.uniform(300.0, 1500.0));
                }
                self.gust_left -= 1;
                self.gust += (self.gust_target - self.gust) * (1.0 - decay(150.0, sr));
                let noise = self.rng.next_f32() * 2.0 - 1.0;
                let rumble = {
                    let t = self.wind_lp[0].process(noise);
                    self.wind_lp[1].process(t)
                };
                y += rumble * self.gust * v[WIND] * 4.0;
            }

            // --- the capsule ---
            if self.low_hz > 20.5 || targets.0 > 20.5 {
                y = {
                    let t = self.hp[0].process(y);
                    self.hp[1].process(t)
                };
            }
            if !(self.high_off(self.high_hz) && self.high_off(targets.1)) {
                y = {
                    let t = self.lp[0].process(y);
                    self.lp[1].process(t)
                };
            }

            // --- electronics ---
            if hiss_level > 0.0 {
                let w = self.rng.next_f32() * 2.0 - 1.0;
                self.pink[0] = 0.99765 * self.pink[0] + w * 0.099_046;
                self.pink[1] = 0.963 * self.pink[1] + w * 0.296_516_4;
                self.pink[2] = 0.57 * self.pink[2] + w * 1.052_691_3;
                let pink = self.pink[0] + self.pink[1] + self.pink[2] + w * 0.1848;
                y += pink * 0.45 * hiss_level;
            }
            if hum_level > 0.0 {
                let x = self.hum_phase * HUM_TABLE as f32;
                let i = x as usize;
                let f = x - i as f32;
                y += hum_level * (self.hum_table[i] + (self.hum_table[i + 1] - self.hum_table[i]) * f);
                self.hum_phase += hum_hz / sr;
                if self.hum_phase >= 1.0 {
                    self.hum_phase -= 1.0;
                }
            }
            if v[CRACKLE] > 0.0 {
                // Crackle comes in bursts: on for a while, then off.
                if self.crackle_left == 0 {
                    self.crackle_on = self.rng.next_f32() < (0.3 + 0.5 * v[CRACKLE]) * self.weather.min(1.5);
                    self.crackle_left = ms(self.sr, self.uniform(100.0, 800.0));
                }
                self.crackle_left -= 1;
                if self.crackle_on && self.chance(120.0 * v[CRACKLE] * v[CRACKLE]) {
                    let sign = if self.rng.next_f32() < 0.5 { -1.0 } else { 1.0 };
                    self.click = sign * self.uniform(0.03, 0.3) * v[CRACKLE];
                    self.click_decay = decay(self.uniform(0.2, 1.5), sr);
                }
                if self.click != 0.0 {
                    y += self.click;
                    self.click *= self.click_decay;
                    if self.click.abs() < 1e-5 {
                        self.click = 0.0;
                    }
                }
            }

            // --- gain stages and the converter ---
            if v[PUMP] > 0.0 {
                self.agc_env = y * y + agc_env_c * (self.agc_env - y * y);
                let level = self.agc_env.sqrt().max(1e-6);
                let want = (0.1 / level).clamp(0.25, agc_max);
                let c = if want < self.agc_gain { agc_down } else { agc_up };
                self.agc_gain = want + c * (self.agc_gain - want);
                y *= self.agc_gain;
            }
            if v[DRIFT] > 0.0 {
                if self.drift_left == 0 {
                    self.drift_target = self.uniform(-9.0, 9.0) * v[DRIFT];
                    self.drift_left = ms(self.sr, self.uniform(300.0, 2000.0));
                }
                self.drift_left -= 1;
                self.drift_db = self.drift_target + drift_c * (self.drift_db - self.drift_target);
                y *= 10f32.powf(self.drift_db / 20.0);
            }
            if v[CLIP] > 0.0 {
                y = y.clamp(-clip_t, clip_t) * clip_makeup;
            }

            // --- gate and connection ---
            if v[GATE] > 0.0 {
                let a = y.abs();
                let c = if a > self.gate_env { attack } else { release };
                self.gate_env = a + c * (self.gate_env - a);
                self.gate_peak = if self.gate_env > self.gate_peak {
                    self.gate_env
                } else {
                    self.gate_env + peak_release * (self.gate_peak - self.gate_env)
                };
                let thr = self.gate_peak * 10f32.powf(gate_thr_db / 20.0);
                if self.gate_env > thr && self.gate_env > 1e-4 {
                    self.gate_gain = (self.gate_gain + gate_open).min(1.0);
                } else {
                    self.gate_gain = (self.gate_gain - gate_close).max(0.0);
                }
                y *= self.gate_gain;
            }
            if v[DROPOUT] > 0.0 {
                if self.drop_left == 0 && self.chance(0.25 * v[DROPOUT]) {
                    self.drop_left = ms(self.sr, self.uniform(50.0, 400.0));
                }
                let ramp = 1.0 / (0.002 * sr);
                if self.drop_left > 0 {
                    self.drop_left -= 1;
                    self.drop_gain = (self.drop_gain - ramp).max(0.0);
                } else {
                    self.drop_gain = (self.drop_gain + ramp).min(1.0);
                }
                y *= self.drop_gain;
            }
            *s = y;
        }
    }

    fn reset(&mut self) {
        self.rng = Rng::new(0x0BAD_311C);
        self.update_left = 0;
        self.tick_left = 0;
        (self.weather, self.weather_target, self.weather_left) = (1.0, 1.0, 0);
        self.hp.iter_mut().chain(self.lp.iter_mut()).chain(self.wind_lp.iter_mut()).for_each(Biquad::reset);
        self.room_peak.reset();
        self.pop_lp.reset();
        self.rustle_lp.reset();
        self.room_buf.iter_mut().for_each(|v| *v = 0.0);
        self.room_pos = 0;
        (self.pop_fast, self.pop_slow) = (0.0, 0.0);
        self.thump_amp = 0.0;
        (self.gust, self.gust_target, self.gust_left) = (0.0, 0.0, 0);
        self.pink = [0.0; 3];
        self.hum_phase = 0.0;
        (self.click, self.crackle_on, self.crackle_left) = (0.0, false, 0);
        (self.agc_env, self.agc_gain) = (0.01, 1.0);
        (self.drift_db, self.drift_target, self.drift_left) = (0.0, 0.0, 0);
        (self.gate_env, self.gate_peak, self.gate_gain) = (0.0, 0.0, 1.0);
        (self.drop_left, self.drop_gain) = (0, 1.0);
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
        let mut fx = BadMic::new(params);
        fx.prepare(48_000.0, block);
        let mut y = x.to_vec();
        y.chunks_mut(block).for_each(|c| fx.process(c));
        y
    }

    fn band_rms(x: &[f32], lo: f32, hi: f32) -> f32 {
        let mut f = [Biquad::default(); 4];
        for b in &mut f[..2] {
            b.set(Shape::HighPass, 48_000.0, lo, 0.707, 0.0);
        }
        for b in &mut f[2..] {
            b.set(Shape::LowPass, 48_000.0, hi, 0.707, 0.0);
        }
        let y: Vec<f32> = x.iter().map(|s| f.iter_mut().fold(*s, |v, b| b.process(v))).collect();
        analysis::rms(&y[4800..])
    }

    /// Speech-like: a vowel, a pause, another vowel.
    fn phrase() -> Vec<f32> {
        let mut x = signals::vowel(48_000, 1.0, 140.0).iter().map(|s| s * 0.6).collect::<Vec<_>>();
        x.extend(signals::silence(48_000, 1.0));
        x.extend(signals::vowel(48_000, 1.0, 170.0).iter().map(|s| s * 0.6));
        x
    }

    #[test]
    fn neutral_is_bit_exact_passthrough() {
        let x = phrase();
        assert_eq!(run(&x, &[], 480), x);
    }

    #[test]
    fn block_size_invariant_with_everything_on() {
        let x = phrase();
        let all: Vec<(&str, f32)> = SPEC
            .params
            .iter()
            .map(|p| (p.key, if p.key == "hum_hz" { 0.0 } else { p.min + (p.max - p.min) * 0.6 }))
            .collect();
        let a = run(&x, &all, 480);
        assert_eq!(a, run(&x, &all, 37));
        assert!(a.iter().all(|v| v.is_finite()) && analysis::peak(&a) < 2.0);
    }

    #[test]
    fn bandwidth_makes_it_thin_and_dull() {
        let x = signals::noise(48_000, 1.0, 0.2, 3);
        let y = run(&x, &[("low_cut", 400.0), ("high_cut", 3000.0)], 480);
        assert!(band_rms(&y, 30.0, 150.0) < band_rms(&x, 30.0, 150.0) * 0.1);
        assert!(band_rms(&y, 7000.0, 15000.0) < band_rms(&x, 7000.0, 15000.0) * 0.1);
        assert!(band_rms(&y, 900.0, 2000.0) > band_rms(&x, 900.0, 2000.0) * 0.6);
    }

    #[test]
    fn hiss_and_hum_fill_the_silence() {
        let x = signals::silence(48_000, 1.0);
        let hiss = analysis::rms_db(&run(&x, &[("hiss", 100.0)], 480)[4800..]);
        assert!((-36.0..-24.0).contains(&hiss), "hiss at 100 %: {hiss} dBFS");
        let quiet = analysis::rms_db(&run(&x, &[("hiss", 10.0)], 480)[4800..]);
        assert!(quiet < -60.0, "hiss at 10 %: {quiet} dBFS");
        for (choice, hz) in [(0.0, 50.0), (1.0, 60.0)] {
            let y = run(&x, &[("hum", 100.0), ("hum_hz", choice)], 480);
            let f0 = analysis::estimate_f0(&y[4800..24_000], 48_000, 30.0, 400.0).unwrap();
            assert!((f0 - hz).abs() < 1.0, "{f0} Hz");
        }
    }

    #[test]
    fn dropouts_and_gate_cut_the_voice() {
        let x = signals::vowel(48_000, 20.0, 150.0).iter().map(|s| s * 0.5).collect::<Vec<_>>();
        let y = run(&x, &[("dropout", 100.0), ("variation", 0.0)], 480);
        let silent = y.chunks(480).filter(|c| analysis::rms(c) < 1e-4).count();
        assert!(silent > 10, "{silent} silent 10 ms windows");

        // The gate chops the quiet end of a decaying note but keeps its loud start.
        let tone: Vec<f32> = signals::vowel(48_000, 1.0, 150.0)
            .iter()
            .enumerate()
            .map(|(i, s)| s * 0.5 * (-(i as f32) / 9600.0).exp())
            .collect();
        let g = run(&tone, &[("gate", 80.0)], 480);
        assert!(analysis::rms(&g[2400..7200]) > analysis::rms(&tone[2400..7200]) * 0.9);
        assert!(analysis::rms(&g[24_000..]) < analysis::rms(&tone[24_000..]) * 0.1);
    }

    #[test]
    fn auto_gain_swells_the_background_in_pauses() {
        // Voice over quiet hiss: in the pause, the background comes up.
        let x: Vec<f32> = phrase().iter().zip(signals::noise(48_000, 3.0, 0.002, 9)).map(|(a, b)| a + b).collect();
        let y = run(&x, &[("pump", 100.0)], 480);
        let pause = |v: &[f32]| analysis::rms(&v[48_000 + 33_600..96_000]);
        assert!(pause(&y) > pause(&x) * 4.0, "{} vs {}", pause(&y), pause(&x));
    }

    #[test]
    fn clipping_flattens_loud_peaks() {
        let x = signals::vowel(48_000, 0.5, 150.0).iter().map(|s| s * 0.8).collect::<Vec<_>>();
        let y = run(&x, &[("clip", 80.0)], 480);
        let crest = |v: &[f32]| analysis::peak(v) / analysis::rms(v);
        assert!(crest(&y) < crest(&x) * 0.6, "{} vs {}", crest(&y), crest(&x));
    }

    #[test]
    fn handling_crackle_wind_and_pops_add_energy() {
        let x = signals::vowel(48_000, 10.0, 150.0).iter().map(|s| s * 0.3).collect::<Vec<_>>();
        for key in ["handling", "crackle", "wind"] {
            let y = run(&x, &[(key, 100.0), ("variation", 0.0)], 480);
            let added: Vec<f32> = y.iter().zip(&x).map(|(a, b)| a - b).collect();
            assert!(analysis::rms(&added) > 0.003, "{key}: {}", analysis::rms(&added));
        }
        // Pops: the onset of a low-heavy burst gets a thump, the steady part doesn't.
        let mut burst = signals::silence(48_000, 0.3);
        burst.extend(signals::vowel(48_000, 0.7, 110.0).iter().map(|s| s * 0.4));
        let y = run(&burst, &[("pops", 100.0)], 480);
        let onset = |v: &[f32]| {
            let mut f = Biquad::default();
            f.set(Shape::LowPass, 48_000.0, 200.0, 0.707, 0.0);
            let low: Vec<f32> = v.iter().map(|s| f.process(*s)).collect();
            analysis::rms(&low[14_400..16_800])
        };
        assert!(onset(&y) > onset(&burst) * 1.3, "{} vs {}", onset(&y), onset(&burst));
        assert!(analysis::max_abs_diff(&y[40_000..], &burst[40_000..]) < 0.05);
    }
}
