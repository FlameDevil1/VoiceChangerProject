//! Bad connection: an unreliable voice call. Lag spikes (audio stops, continues late, then skips
//! or speeds up to catch up), packet loss (pieces go missing or repeat), robotic jitter glitches,
//! cut-outs, falling behind and catching up, freeze/loop, robotic voice, compression artifacts
//! and a low-quality codec.
//!
//! Everything plays from a history of the input. While no problem is happening the output *is*
//! the input: no delay, bit-exact. Delay exists only during a lag or catch-up, and it is the
//! effect's intent, not processing latency, so it is not reported as latency. Problems are random
//! events started on a 10 ms tick at absolute sample positions, so the output never depends on
//! how audio is split into blocks. Switching between live audio, silence and replayed audio is
//! always crossfaded. Robotic voice and compression artifacts need a short spectral stage
//! (`dsp::spectral`), which adds a fixed 10.7 ms of delay while either control is above 0.

use crate::dsp::Processor;
use crate::dsp::biquad::{Biquad, Shape};
use crate::dsp::params::{EffectParams, EffectSpec, ParamSpec};
use crate::dsp::spectral::{FRAME, SpectralCodec};
use crate::dsp::util::{HannTable, Rng, SmoothedValue};
use crate::dsp::wsola::Wsola;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

const fn pct(key: &'static str, label: &'static str, default: f32, help: &'static str) -> ParamSpec {
    ParamSpec { key, label, min: 0.0, max: 100.0, default, unit: " %", step: 1.0, help }
}

pub const SPEC: EffectSpec = EffectSpec {
    label: "Bad connection",
    help: "An unreliable voice call: lag, stutter, robotic glitches, cut-outs and a low-quality codec. Adds delay only while a lag is happening.",
    params: &[
        pct(
            "amount",
            "Overall",
            40.0,
            "How bad the connection is: scales how often and how badly every problem below happens.",
        ),
        pct("lag", "Lag spikes", 30.0, "Audio stops, continues late, then skips or speeds up to catch up."),
        ParamSpec {
            key: "lag_ms",
            label: "Longest lag",
            min: 50.0,
            max: 2000.0,
            default: 700.0,
            unit: " ms",
            step: 10.0,
            help: "Upper limit for how long a lag spike lasts.",
        },
        pct("loss", "Packet loss", 40.0, "Stuttering: pieces of audio go missing or repeat."),
        ParamSpec {
            key: "packet_ms",
            label: "Packet size",
            min: 10.0,
            max: 120.0,
            default: 40.0,
            unit: " ms",
            step: 5.0,
            help: "Length of each missing or repeated piece.",
        },
        pct("jitter", "Jitter", 30.0, "Short metallic warbles when audio arrives late or out of order."),
        pct(
            "robotic",
            "Robotic voice",
            0.0,
            "Stretches where your voice turns metallic and synthetic, like bad text-to-speech. Adds 10 ms of delay while above 0.",
        ),
        pct("choppy", "Cut-outs", 30.0, "Short, abrupt silences mid-word."),
        pct("drift", "Fall behind", 20.0, "Audio slowly falls behind, then speeds up to catch up."),
        pct("freeze", "Freeze / loop", 15.0, "The last moment repeats a few times before audio resumes."),
        pct(
            "artifacts",
            "Compression",
            0.0,
            "Low-bitrate artifacts: watery, swirly, underwater sound with missing detail. Adds 10 ms of delay while above 0.",
        ),
        ParamSpec {
            key: "bits",
            label: "Codec bits",
            min: 2.0,
            max: 16.0,
            default: 16.0,
            unit: " bit",
            step: 1.0,
            help: "Lower = gritty, crushed audio. 16 = off.",
        },
        ParamSpec {
            key: "codec_rate",
            label: "Codec rate",
            min: 2000.0,
            max: 48000.0,
            default: 48000.0,
            unit: " Hz",
            step: 500.0,
            help: "Lower = muffled, underwater sound. 48000 = off.",
        },
        pct(
            "variation",
            "Variation",
            50.0,
            "How much problems come and go: 0 = steady, 100 = calm stretches and bad patches.",
        ),
    ],
    mix_label: "Mix",
    default_mix: 1.0,
    send_mix: false,
    choices: &[],
    random: &[
        ("lag", 0.0, 80.0),
        ("loss", 0.0, 80.0),
        ("jitter", 0.0, 80.0),
        ("choppy", 0.0, 80.0),
        ("drift", 0.0, 60.0),
        ("freeze", 0.0, 50.0),
        ("robotic", 0.0, 60.0),
        ("artifacts", 0.0, 60.0),
    ],
    presets: &[
        ("Mild", &[("amount", 20.0)]),
        ("Bad", &[("amount", 55.0), ("codec_rate", 16000.0)]),
        (
            "Unusable",
            &[("amount", 95.0), ("robotic", 50.0), ("artifacts", 70.0), ("codec_rate", 8000.0), ("variation", 30.0)],
        ),
        ("Low bitrate", &[("amount", 10.0), ("artifacts", 60.0), ("codec_rate", 12000.0)]),
        (
            "Robotic",
            &[("amount", 60.0), ("robotic", 90.0), ("lag", 0.0), ("loss", 20.0), ("choppy", 10.0), ("drift", 0.0)],
        ),
        (
            "Laggy",
            &[("amount", 60.0), ("lag", 90.0), ("loss", 10.0), ("jitter", 0.0), ("choppy", 0.0), ("drift", 60.0)],
        ),
        (
            "Stuttery",
            &[("amount", 60.0), ("lag", 0.0), ("loss", 90.0), ("jitter", 60.0), ("drift", 0.0), ("freeze", 0.0)],
        ),
    ],
};

/// `status` while a problem (or a requested glitch) is playing.
pub const STATUS_BUSY: u32 = 1;

const AMOUNT: usize = 0;
const LAG: usize = 1;
const LAG_MS: usize = 2;
const LOSS: usize = 3;
const PACKET_MS: usize = 4;
const JITTER: usize = 5;
const ROBOTIC: usize = 6;
const CHOPPY: usize = 7;
const DRIFT: usize = 8;
const FREEZE: usize = 9;
const ARTIFACTS: usize = 10;
const BITS: usize = 11;
const CODEC_RATE: usize = 12;
const VARIATION: usize = 13;

/// The problems that happen as random events, in `RATE` order.
const EVENTS: [usize; 7] = [LAG, LOSS, JITTER, ROBOTIC, CHOPPY, DRIFT, FREEZE];
/// Events per second at 100 % weight and 100 % overall amount.
const RATE: [f32; 7] = [0.08, 0.25, 0.2, 0.06, 0.6, 0.04, 0.08];
/// Longest lag the history is sized for.
const MAX_LAG_S: f32 = 2.0;
/// Playback speed while falling behind and while catching up.
const SLOW: f64 = 0.9;
const FAST: f64 = 1.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    Live,
    Silence,
    Reader,
    Loop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    /// Silence while audio "doesn't arrive"; then play from `from`, late, for `hold` samples.
    Stall {
        left: u32,
        from: u64,
        hold: u32,
        speed_up: bool,
    },
    /// Playing late at normal speed; then jump or speed up back to live.
    Behind {
        left: u32,
        speed_up: bool,
    },
    /// Playing slower (`slow`) until `target` samples behind, then faster until caught up.
    Drift {
        slow: bool,
        target: f64,
    },
    /// Packet loss burst: each packet is played, repeated or dropped.
    Stutter {
        left: u32,
        packet_left: u32,
        lost_run: u32,
    },
    /// Robotic burst: frames replaced by a tiny looped fragment. `glitch` leads into a lag spike.
    Warble {
        left: u32,
        frame_left: u32,
        glitch: bool,
    },
    Chop {
        left: u32,
    },
    Freeze {
        left: u32,
    },
}

/// Plays `len` samples of history from `start` over and over, crossfading each wrap.
#[derive(Clone, Copy, Debug)]
struct Looper {
    start: i64,
    len: usize,
    xf: usize,
    pos: usize,
}

impl Looper {
    fn new(start: i64, len: usize, xf: usize) -> Self {
        let len = len.max(4);
        Self { start, len, xf: xf.clamp(1, len / 2 - 1), pos: 0 }
    }

    fn next(&mut self, h: &[f32], mask: usize) -> f32 {
        let at = |i: usize| h[(self.start + i as i64) as usize & mask];
        let tail = self.len - self.xf;
        let mut y = at(self.pos);
        if self.pos >= tail {
            let a = (self.pos - tail) as f32 / self.xf as f32;
            y += (at(self.pos - tail) - y) * a;
        }
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = self.xf;
        }
        y
    }
}

/// Duration in samples (at least 1).
fn samples(sr: f32, ms: f32) -> u32 {
    (ms * 0.001 * sr).max(1.0) as u32
}

pub struct Network {
    params: Arc<EffectParams>,
    sr: f32,
    hist: Vec<f32>,
    mask: usize,
    n: u64,
    hann: HannTable,
    reader: Wsola,
    looper: Looper,
    cur: Src,
    prev: Src,
    fade_left: u32,
    fade_len: u32,
    phase: Phase,
    rng: Rng,
    tick_len: u32,
    tick_left: u32,
    weather: f32,
    weather_target: f32,
    weather_left: u32,
    seen_trigger: u32,
    glitch_pending: bool,
    // Codec: anti-alias filter, sample-and-hold downsampling, then bit reduction.
    lp: [Biquad; 2],
    lp_hz: f32,
    // Spectral stage: robotic voice bursts and compression artifacts.
    spectral: SpectralCodec,
    spectral_mix: SmoothedValue,
    spectral_active: bool,
    robot_left: u32,
    robot_mix: SmoothedValue,
    hold: f32,
    hold_phase: f32,
}

impl Network {
    pub fn new(params: Arc<EffectParams>) -> Self {
        let seen_trigger = params.trigger.load(Relaxed);
        Self {
            params,
            sr: 48_000.0,
            hist: Vec::new(),
            mask: 0,
            n: 0,
            hann: HannTable::new(),
            reader: Wsola::new(48_000.0),
            looper: Looper::new(0, 16, 2),
            cur: Src::Live,
            prev: Src::Live,
            fade_left: 0,
            fade_len: 1,
            phase: Phase::Idle,
            rng: Rng::new(0xBAD_C0FFEE),
            tick_len: 480,
            tick_left: 0,
            weather: 1.0,
            weather_target: 1.0,
            weather_left: 0,
            seen_trigger,
            glitch_pending: false,
            lp: [Biquad::default(); 2],
            lp_hz: 0.0,
            hold: 0.0,
            hold_phase: 0.0,
            spectral: SpectralCodec::new(48_000.0),
            spectral_mix: SmoothedValue::new(0.0, 48_000.0, 0.02),
            spectral_active: false,
            robot_left: 0,
            robot_mix: SmoothedValue::new(0.0, 48_000.0, 0.05),
        }
    }

    /// Robotic voice or compression is set, so the spectral stage must run.
    fn spectral_wanted(&self) -> bool {
        self.params.get(ARTIFACTS) > 0.0 || self.params.get(ROBOTIC) > 0.0
    }

    /// Uniform random in [lo, hi).
    fn uniform(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.rng.next_f32()
    }

    fn set_source(&mut self, src: Src, fade_ms: f32) {
        if src != self.cur {
            self.prev = self.cur;
            self.cur = src;
            self.fade_len = samples(self.sr, fade_ms);
            self.fade_left = self.fade_len;
        }
    }

    fn render(&mut self, src: Src, x: f32) -> f32 {
        match src {
            Src::Live => x,
            Src::Silence => 0.0,
            Src::Reader => self.reader.next(&self.hist, self.mask, self.n as i64 + 1, &self.hann),
            Src::Loop => self.looper.next(&self.hist, self.mask),
        }
    }

    fn start_loop(&mut self, len: u32, xf_ms: f32) {
        let xf = samples(self.sr, xf_ms) as usize;
        self.looper = Looper::new(self.n as i64 + 1 - len as i64, len as usize, xf);
    }

    fn start_lag(&mut self, ms: f32, hold_s: f32, speed_up: bool) {
        let left = samples(self.sr, ms.clamp(50.0, MAX_LAG_S * 1000.0));
        let hold = samples(self.sr, hold_s * 1000.0);
        self.phase = Phase::Stall { left, from: self.n, hold, speed_up };
        self.set_source(Src::Silence, 5.0);
    }

    fn start_event(&mut self, which: usize, amount: f32) {
        let sev = 0.4 + 0.6 * amount;
        let lag_ms = self.params.get(LAG_MS);
        match EVENTS[which] {
            LAG => {
                let ms = lag_ms * self.uniform(0.3, 1.0) * sev;
                let hold = self.uniform(0.4, 2.5);
                let speed_up = self.rng.next_f32() < 0.5;
                self.start_lag(ms, hold, speed_up);
            }
            LOSS => {
                let left = samples(self.sr, self.uniform(400.0, 1600.0) * sev);
                self.phase = Phase::Stutter { left, packet_left: 0, lost_run: 0 };
            }
            JITTER => {
                let left = samples(self.sr, self.uniform(300.0, 1200.0) * sev);
                self.phase = Phase::Warble { left, frame_left: 0, glitch: false };
            }
            ROBOTIC => {
                // Runs alongside other problems; leaves the phase idle.
                self.robot_left = samples(self.sr, self.uniform(800.0, 3000.0) * sev);
            }
            CHOPPY => {
                let left = samples(self.sr, self.uniform(20.0, 80.0) * (0.5 + 0.5 * sev));
                self.phase = Phase::Chop { left };
                self.set_source(Src::Silence, 1.0);
            }
            DRIFT => {
                let d0 = self.reader.min_delay() + samples(self.sr, 10.0) as f64;
                self.reader.start(self.n as i64 - d0 as i64, SLOW);
                let target = samples(self.sr, self.uniform(150.0, 600.0) * sev) as f64;
                self.phase = Phase::Drift { slow: true, target: target.max(2.0 * d0) };
                self.set_source(Src::Reader, 10.0);
            }
            _ => {
                let seg = samples(self.sr, self.uniform(80.0, 250.0));
                let reps = 2 + (self.rng.next_f32() * 3.0) as u32;
                self.start_loop(seg, 5.0);
                self.phase = Phase::Freeze { left: seg * reps };
                self.set_source(Src::Loop, 5.0);
            }
        }
    }

    /// Every 10 ms: drift the "weather" (how bad things are right now) and maybe start a problem.
    fn on_tick(&mut self, amount: f32, weights: &[f32; 7], variation: f32) {
        if self.weather_left == 0 {
            self.weather_target = 1.0 + variation * (2.0 * self.rng.next_f32() - 1.0);
            self.weather_left = 300 + (self.rng.next_f32() * 700.0) as u32;
        }
        self.weather_left -= 1;
        self.weather += (self.weather_target - self.weather) * 0.005;

        if self.phase != Phase::Idle || self.fade_left > 0 {
            return;
        }
        if self.glitch_pending {
            self.glitch_pending = false;
            let left = samples(self.sr, 600.0);
            self.phase = Phase::Warble { left, frame_left: 0, glitch: true };
            return;
        }
        if amount <= 0.0 {
            return;
        }
        let scale = amount.powf(1.5) * self.weather.max(0.0) * 0.01;
        let r = self.rng.next_f32();
        let mut acc = 0.0;
        for (i, w) in weights.iter().enumerate() {
            acc += w * RATE[i] * scale;
            if r < acc {
                self.start_event(i, amount);
                return;
            }
        }
    }

    /// Advance the current problem by one sample (before rendering it).
    fn step_phase(&mut self, amount: f32, packet: u32) {
        match self.phase {
            Phase::Idle => {}
            Phase::Stall { left, from, hold, speed_up } => {
                if left <= 1 {
                    self.reader.start(from as i64, 1.0);
                    self.set_source(Src::Reader, 5.0);
                    self.phase = Phase::Behind { left: hold, speed_up };
                } else {
                    self.phase = Phase::Stall { left: left - 1, from, hold, speed_up };
                }
            }
            Phase::Behind { left, speed_up } => {
                if left <= 1 {
                    if speed_up {
                        self.reader.rate = FAST;
                        self.phase = Phase::Drift { slow: false, target: 0.0 };
                    } else {
                        // Skip ahead: what was said meanwhile is lost, like a real call.
                        self.set_source(Src::Live, 15.0);
                        self.phase = Phase::Idle;
                    }
                } else {
                    self.phase = Phase::Behind { left: left - 1, speed_up };
                }
            }
            Phase::Drift { slow, target } => {
                let delay = (self.n + 1) as f64 - self.reader.position();
                if slow && delay >= target {
                    self.reader.rate = FAST;
                    self.phase = Phase::Drift { slow: false, target };
                } else if !slow && delay <= self.reader.min_delay() + samples(self.sr, 5.0) as f64 {
                    self.set_source(Src::Live, 10.0);
                    self.phase = Phase::Idle;
                }
            }
            Phase::Stutter { left, packet_left, lost_run } => {
                if left <= 1 {
                    self.set_source(Src::Live, 2.0);
                    self.phase = Phase::Idle;
                    return;
                }
                let (mut packet_left, mut lost_run) = (packet_left, lost_run);
                if packet_left == 0 {
                    packet_left = packet;
                    if self.rng.next_f32() < 0.3 + 0.4 * amount {
                        if lost_run == 0 {
                            // Concealment: the previous packet plays again.
                            self.start_loop(packet, 3.0);
                            self.set_source(Src::Loop, 2.0);
                        } else {
                            self.set_source(Src::Silence, 2.0);
                        }
                        lost_run += 1;
                    } else {
                        lost_run = 0;
                        self.set_source(Src::Live, 2.0);
                    }
                }
                self.phase = Phase::Stutter { left: left - 1, packet_left: packet_left - 1, lost_run };
            }
            Phase::Warble { left, frame_left, glitch } => {
                if left <= 1 {
                    self.set_source(Src::Live, 2.0);
                    self.phase = Phase::Idle;
                    if glitch {
                        let ms = self.uniform(300.0, 700.0);
                        let hold = self.uniform(0.3, 0.8);
                        self.start_lag(ms, hold, false);
                    }
                    return;
                }
                let mut frame_left = frame_left;
                if frame_left == 0 {
                    frame_left = samples(self.sr, 20.0);
                    if self.rng.next_f32() < 0.5 + 0.3 * amount {
                        if self.cur != Src::Loop {
                            let seg = samples(self.sr, self.uniform(5.0, 12.0));
                            self.start_loop(seg, 1.0);
                            self.set_source(Src::Loop, 2.0);
                        }
                    } else {
                        self.set_source(Src::Live, 2.0);
                    }
                }
                self.phase = Phase::Warble { left: left - 1, frame_left: frame_left - 1, glitch };
            }
            Phase::Chop { left } => {
                if left <= 1 {
                    self.set_source(Src::Live, 1.0);
                    self.phase = Phase::Idle;
                } else {
                    self.phase = Phase::Chop { left: left - 1 };
                }
            }
            Phase::Freeze { left } => {
                if left <= 1 {
                    self.set_source(Src::Live, 10.0);
                    self.phase = Phase::Idle;
                } else {
                    self.phase = Phase::Freeze { left: left - 1 };
                }
            }
        }
    }
}

impl Processor for Network {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sr = sample_rate;
        // Longest lag plus catch-up margin and the reader's grains.
        let size = ((sample_rate * (MAX_LAG_S + 0.5)) as usize).next_power_of_two();
        self.hist = vec![0.0; size];
        self.mask = size - 1;
        self.reader = Wsola::new(sample_rate);
        self.tick_len = samples(self.sr, 10.0);
        self.lp_hz = 0.0;
        self.spectral = SpectralCodec::new(sample_rate);
        // Start in the requested state (a chain built with it on starts on, no fade).
        let on = self.spectral_wanted();
        self.spectral_mix = SmoothedValue::new(if on { 1.0 } else { 0.0 }, sample_rate, 0.02);
        self.spectral_active = on;
        self.robot_mix = SmoothedValue::new(0.0, sample_rate, 0.05);
        self.reset();
    }

    fn latency(&self) -> usize {
        if self.spectral_active { FRAME } else { 0 }
    }

    fn max_latency(&self) -> usize {
        FRAME
    }

    fn process(&mut self, buf: &mut [f32]) {
        let p = &self.params;
        let amount = p.get(AMOUNT) / 100.0;
        let weights: [f32; 7] = std::array::from_fn(|i| p.get(EVENTS[i]) / 100.0);
        let artifacts = p.get(ARTIFACTS) / 100.0;
        let wanted = self.spectral_wanted();
        self.spectral_mix.set_target(if wanted { 1.0 } else { 0.0 });
        self.spectral_active = wanted || !self.spectral_mix.is_settled() || self.spectral_mix.current() > 0.0;
        let spectral = self.spectral_active;
        let variation = p.get(VARIATION) / 100.0;
        let packet = samples(self.sr, p.get(PACKET_MS));
        let trigger = p.trigger.load(Relaxed);
        if trigger != self.seen_trigger {
            self.seen_trigger = trigger;
            self.glitch_pending = true;
        }
        let bits = p.get(BITS);
        let codec_rate = p.get(CODEC_RATE);
        let crush = bits < 15.5;
        let downsample = codec_rate < self.sr * 0.98;
        if downsample && codec_rate != self.lp_hz {
            self.lp_hz = codec_rate;
            for f in &mut self.lp {
                f.set(Shape::LowPass, self.sr, 0.45 * codec_rate, 0.707, 0.0);
            }
        }
        let q = 2f32.powf(bits.round() - 1.0);
        let hold_step = codec_rate / self.sr;

        for s in buf.iter_mut() {
            let x = *s;
            self.hist[self.n as usize & self.mask] = x;
            if self.tick_left == 0 {
                self.tick_left = self.tick_len;
                self.on_tick(amount, &weights, variation);
            }
            self.tick_left -= 1;
            self.step_phase(amount, packet);

            let now = self.render(self.cur, x);
            let mut y = if self.fade_left > 0 {
                let before = self.render(self.prev, x);
                let g = 1.0 - self.fade_left as f32 / self.fade_len as f32;
                self.fade_left -= 1;
                before + (now - before) * g
            } else {
                now
            };
            self.n += 1;

            // Robotic bursts fade in and out; the codec reads the depth at each frame.
            if self.robot_left > 0 {
                self.robot_left -= 1;
            }
            self.robot_mix.set_target(if self.robot_left > 0 { 1.0 } else { 0.0 });
            let robot = self.robot_mix.next_value();
            let shaped = self.spectral.next(y, spectral, artifacts, robot);
            if spectral {
                let m = self.spectral_mix.next_value();
                y += (shaped - y) * m;
            }

            if downsample {
                let f = {
                    let t = self.lp[0].process(y);
                    self.lp[1].process(t)
                };
                self.hold_phase += hold_step;
                if self.hold_phase >= 1.0 {
                    self.hold_phase -= 1.0;
                    self.hold = f;
                }
                y = self.hold;
            }
            if crush {
                y = (y * q).round() / q;
            }
            *s = y;
        }

        let delay = if self.cur == Src::Reader { (self.n as f64 - self.reader.position()).max(0.0) } else { 0.0 };
        self.params.meter.store((delay / self.sr as f64 * 1000.0) as f32);
        let busy = self.phase != Phase::Idle || self.fade_left > 0 || self.glitch_pending || self.robot_left > 0;
        self.params.status.store(if busy { STATUS_BUSY } else { 0 }, Relaxed);
    }

    fn reset(&mut self) {
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.n = 0;
        self.cur = Src::Live;
        self.prev = Src::Live;
        self.fade_left = 0;
        self.phase = Phase::Idle;
        self.rng = Rng::new(0xBAD_C0FFEE);
        self.tick_left = 0;
        self.weather = 1.0;
        self.weather_target = 1.0;
        self.weather_left = 0;
        self.lp.iter_mut().for_each(Biquad::reset);
        self.hold = 0.0;
        self.hold_phase = 0.0;
        self.spectral.reset();
        self.robot_left = 0;
        self.robot_mix.set_target(0.0);
        self.robot_mix.snap();
        self.params.meter.store(0.0);
        self.params.status.store(0, Relaxed);
        // `seen_trigger` is kept: a glitch requested while off plays once the effect is on.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    fn params(settings: &[(&str, f32)]) -> Arc<EffectParams> {
        let params = Arc::new(EffectParams::new(&SPEC));
        for (k, v) in settings {
            params.set(SPEC.index(k).unwrap(), *v);
        }
        params
    }

    fn run(x: &[f32], settings: &[(&str, f32)], block: usize) -> Vec<f32> {
        let mut fx = Network::new(params(settings));
        fx.prepare(48_000.0, block);
        let mut y = x.to_vec();
        y.chunks_mut(block).for_each(|c| fx.process(c));
        y
    }

    /// Number of 20 ms windows where the input is loud but the output is (almost) silent.
    fn gaps(x: &[f32], y: &[f32]) -> usize {
        x.chunks(960).zip(y.chunks(960)).filter(|(a, b)| analysis::rms(a) > 0.05 && analysis::rms(b) < 0.002).count()
    }

    const CALM: [(&str, f32); 2] = [("amount", 0.0), ("variation", 0.0)];

    #[test]
    fn calm_connection_is_bit_exact_passthrough() {
        let x = signals::vowel(48_000, 1.0, 140.0);
        assert_eq!(run(&x, &CALM, 480), x);
    }

    #[test]
    fn block_size_invariant_with_everything_going_wrong() {
        let x = signals::vowel_wobble(48_000, 8.0, 150.0, 0.1, 2.0);
        let all = [
            ("amount", 100.0),
            ("lag", 100.0),
            ("loss", 100.0),
            ("jitter", 100.0),
            ("choppy", 100.0),
            ("drift", 100.0),
            ("freeze", 100.0),
            ("bits", 10.0),
            ("codec_rate", 12000.0),
            ("robotic", 100.0),
            ("artifacts", 50.0),
        ];
        let a = run(&x, &all, 480);
        assert_eq!(a, run(&x, &all, 37));
        assert!(a.iter().all(|v| v.is_finite()));
        assert!(analysis::peak(&a) < analysis::peak(&x) * 1.3, "{}", analysis::peak(&a));
        assert!(analysis::max_abs_diff(&a, &x) > 0.1, "nothing happened");
        assert!(gaps(&x, &a) > 0, "no dropouts in 8 s of a terrible connection");
    }

    #[test]
    fn reader_at_normal_speed_is_a_clean_delayed_copy() {
        let x = signals::vowel(48_000, 0.5, 170.0);
        let mut h = vec![0.0f32; 1 << 16];
        let mask = h.len() - 1;
        let hann = HannTable::new();
        let mut r = Wsola::new(48_000.0);
        let d = 3000usize;
        r.start(0, 1.0);
        let mut y = Vec::new();
        for (n, &v) in x.iter().enumerate() {
            h[n & mask] = v;
            if n >= d {
                y.push(r.next(&h, mask, n as i64 + 1, &hann));
            }
        }
        // The first grain fades in over one hop; after that it is the input, `d` samples late.
        let skip = r.hop;
        assert!(analysis::max_abs_diff(&y[skip..], &x[skip..y.len()]) < 1e-5);
    }

    #[test]
    fn speeding_up_and_slowing_down_keep_the_pitch() {
        let x = signals::vowel(48_000, 3.0, 200.0);
        for rate in [SLOW, FAST] {
            let mut h = vec![0.0f32; 1 << 18];
            let mask = h.len() - 1;
            let hann = HannTable::new();
            let mut r = Wsola::new(48_000.0);
            let d = 48_000usize; // start 1 s behind so a fast reader never catches up
            r.start(0, rate);
            let mut y = Vec::new();
            for (n, &v) in x.iter().enumerate() {
                h[n & mask] = v;
                if n >= d {
                    y.push(r.next(&h, mask, n as i64 + 1, &hann));
                }
            }
            let f0 = analysis::estimate_f0(&y[4800..24_000], 48_000, 50.0, 800.0).unwrap();
            assert!((f0 - 200.0).abs() < 4.0, "rate {rate}: f0 {f0}");
            let read = r.position();
            let expected = rate * y.len() as f64;
            assert!((read - expected).abs() < 1500.0, "rate {rate}: read {read} vs {expected}");
        }
    }

    #[test]
    fn lag_spikes_add_delay_then_return_to_live() {
        let x = signals::vowel_wobble(48_000, 60.0, 150.0, 0.1, 2.0);
        let settings = [
            ("amount", 100.0),
            ("lag", 100.0),
            ("loss", 0.0),
            ("jitter", 0.0),
            ("choppy", 0.0),
            ("drift", 0.0),
            ("freeze", 0.0),
            ("variation", 0.0),
        ];
        let mut fx = Network::new(params(&settings));
        fx.prepare(48_000.0, 480);
        let mut y = x.clone();
        let mut max_delay = 0.0f32;
        for c in y.chunks_mut(480) {
            fx.process(c);
            max_delay = max_delay.max(fx.params.meter.load());
        }
        assert!((50.0..2100.0).contains(&max_delay), "max delay {max_delay} ms");
        assert!(gaps(&x, &y) > 2, "a lag spike starts with silence");
    }

    #[test]
    fn glitch_trigger_plays_a_burst_then_goes_back_to_exact_live() {
        let x = signals::vowel(48_000, 4.0, 150.0);
        let params = params(&CALM);
        let mut fx = Network::new(params.clone());
        fx.prepare(48_000.0, 480);
        let mut y = x.clone();
        for (i, c) in y.chunks_mut(480).enumerate() {
            if i == 25 {
                params.trigger.fetch_add(1, Relaxed);
            }
            fx.process(c);
        }
        assert_eq!(&y[..12_000], &x[..12_000], "untouched before the trigger");
        assert!(gaps(&x, &y) >= 10, "a lag of 300+ ms follows the warble: {}", gaps(&x, &y));
        let tail = 48_000;
        assert_eq!(&y[y.len() - tail..], &x[x.len() - tail..], "back to exact live audio");
    }

    #[test]
    fn spectral_stage_alone_is_a_clean_10ms_delay() {
        // Robotic voice set but no events (amount 0): the stage runs and changes nothing.
        let x = signals::vowel(48_000, 1.0, 140.0);
        let mut fx = Network::new(params(&[("amount", 0.0), ("robotic", 50.0)]));
        fx.prepare(48_000.0, 480);
        assert_eq!(fx.latency(), FRAME);
        let mut y = x.clone();
        y.chunks_mut(480).for_each(|c| fx.process(c));
        assert!(analysis::max_abs_diff(&y[FRAME..], &x[..x.len() - FRAME]) < 1e-4);

        let mut off = Network::new(params(&CALM));
        off.prepare(48_000.0, 480);
        assert_eq!(off.latency(), 0, "no delay unless robotic voice or compression is used");
    }

    #[test]
    fn compression_removes_detail_but_keeps_the_voice() {
        let x = signals::noise(48_000, 1.0, 0.3, 4);
        let y = run(&x, &[("amount", 0.0), ("artifacts", 100.0)], 480);
        let band = |v: &[f32], lo: f32, hi: f32| {
            let mut f = [Biquad::default(); 2];
            f[0].set(Shape::HighPass, 48_000.0, lo, 0.707, 0.0);
            f[1].set(Shape::LowPass, 48_000.0, hi, 0.707, 0.0);
            analysis::rms(
                &v.iter()
                    .map(|s| {
                        let t = f[0].process(*s);
                        f[1].process(t)
                    })
                    .collect::<Vec<_>>()[4800..],
            )
        };
        assert!(band(&y, 9000.0, 20000.0) < band(&x, 9000.0, 20000.0) * 0.2, "treble kept");
        assert!(band(&y, 300.0, 2000.0) > band(&x, 300.0, 2000.0) * 0.3, "voice band lost");
        // Holes come and go: frames differ in which bins survive, unlike the input's steady noise.
        assert!(analysis::rms(&y[4800..]) > analysis::rms(&x[4800..]) * 0.2);
    }

    #[test]
    fn robotic_voice_buzzes_at_the_frame_rate() {
        let x = signals::vowel(48_000, 30.0, 140.0);
        let settings = [
            ("amount", 100.0),
            ("robotic", 100.0),
            ("lag", 0.0),
            ("loss", 0.0),
            ("jitter", 0.0),
            ("choppy", 0.0),
            ("drift", 0.0),
            ("freeze", 0.0),
            ("variation", 0.0),
        ];
        let y = run(&x, &settings, 480);
        let buzz = 48_000.0 / crate::dsp::spectral::HOP as f32;
        let robotic = y
            .chunks(4800)
            .filter_map(|c| analysis::estimate_f0(c, 48_000, 50.0, 400.0))
            .filter(|f| (f - buzz).abs() < 6.0)
            .count();
        assert!(robotic >= 3, "{robotic} windows at the {buzz} Hz robot pitch");
    }

    #[test]
    fn codec_muffles_and_crushes() {
        let x = signals::noise(48_000, 1.0, 0.3, 5);
        let hf = |v: &[f32]| {
            let mut f = Biquad::default();
            f.set(Shape::HighPass, 48_000.0, 6000.0, 0.707, 0.0);
            analysis::rms(&v.iter().map(|s| f.process(*s)).collect::<Vec<_>>())
        };
        let muffled = run(&x, &[("amount", 0.0), ("codec_rate", 8000.0)], 480);
        assert!(hf(&muffled) < hf(&x) * 0.3, "{} vs {}", hf(&muffled), hf(&x));
        let crushed = run(&x, &[("amount", 0.0), ("bits", 4.0)], 480);
        let mut levels: Vec<i32> = crushed.iter().map(|v| (v * 8.0).round() as i32).collect();
        levels.sort();
        levels.dedup();
        assert!(levels.len() <= 17, "4 bits gives at most 16 levels (+ clipping edge): {}", levels.len());
        assert!(crushed.iter().all(|v| (v * 8.0 - (v * 8.0).round()).abs() < 1e-6));
    }
}
