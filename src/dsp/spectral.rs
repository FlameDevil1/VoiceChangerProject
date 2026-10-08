//! Streaming STFT stage for connection artifacts: what a starved codec does to a voice.
//!
//! - **Compression artifacts**: the bandwidth is cut and flickers frame to frame, weak parts of
//!   each band are dropped (spectral holes), mid-level detail drops in and out at random and
//!   levels are quantised coarsely. The holes moving between frames are the "watery", "swirly",
//!   underwater sound of a low-bitrate stream.
//! - **Robotic voice**: every frame's phases are pulled towards a fixed pattern. The voice turns
//!   into a buzz at the frame rate (sample rate / hop, ~188 Hz at 48 kHz) that keeps the words'
//!   spectral shape: metallic and synthetic, like bad text-to-speech.
//!
//! 512-sample frames, 50 % overlap, square-root Hann windows on both sides: with nothing applied
//! the output is the input delayed by exactly `FRAME` samples. Frames start at absolute sample
//! positions, so the output does not depend on block size. Never allocates after `new`.

use super::fft::fft_in_place;
use super::util::Rng;
use std::f32::consts::PI;

pub const FRAME: usize = 512;
pub const HOP: usize = FRAME / 2;
const BINS: usize = FRAME / 2 + 1;

pub struct SpectralCodec {
    sr: f32,
    window: Vec<f32>,
    input: Vec<f32>,
    in_pos: usize,
    /// Overlap-add accumulator and the finished hop being played out.
    acc: Vec<f32>,
    ready: Vec<f32>,
    hop_pos: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Bin ranges for masking, roughly a third of an octave wide (at least 2 bins).
    bands: Vec<(usize, usize)>,
    rng: Rng,
}

impl SpectralCodec {
    pub fn new(sample_rate: f32) -> Self {
        let window = (0..FRAME).map(|i| (PI * i as f32 / FRAME as f32).sin()).collect();
        let mut bands = Vec::new();
        let mut lo = 1;
        while lo < BINS {
            let hi = (lo + (lo / 4).max(2)).min(BINS);
            bands.push((lo, hi));
            lo = hi;
        }
        Self {
            sr: sample_rate,
            window,
            input: vec![0.0; FRAME],
            in_pos: 0,
            acc: vec![0.0; FRAME],
            ready: vec![0.0; HOP],
            hop_pos: 0,
            re: vec![0.0; FRAME],
            im: vec![0.0; FRAME],
            bands,
            rng: Rng::new(0xC0DE_C5),
        }
    }

    pub fn reset(&mut self) {
        self.input.iter_mut().chain(self.acc.iter_mut()).chain(self.ready.iter_mut()).for_each(|v| *v = 0.0);
        self.in_pos = 0;
        self.hop_pos = 0;
        self.rng = Rng::new(0xC0DE_C5);
    }

    /// Feed one sample, get one sample `FRAME` samples late. `active = false` only records input
    /// (so switching on later starts from real audio) and returns 0. `artifacts` and `robot` are
    /// 0..1 and are read at frame boundaries.
    pub fn next(&mut self, x: f32, active: bool, artifacts: f32, robot: f32) -> f32 {
        self.input[self.in_pos] = x;
        self.in_pos = (self.in_pos + 1) % FRAME;
        let y = self.ready[self.hop_pos];
        self.hop_pos += 1;
        if self.hop_pos == HOP {
            self.hop_pos = 0;
            if active {
                self.frame(artifacts, robot);
            } else {
                self.acc.iter_mut().chain(self.ready.iter_mut()).for_each(|v| *v = 0.0);
            }
        }
        if active { y } else { 0.0 }
    }

    fn frame(&mut self, artifacts: f32, robot: f32) {
        // Oldest sample first: `in_pos` is where the next sample will go.
        for i in 0..FRAME {
            self.re[i] = self.input[(self.in_pos + i) % FRAME] * self.window[i];
            self.im[i] = 0.0;
        }
        fft_in_place(&mut self.re, &mut self.im);
        if artifacts > 0.0 {
            self.compress(artifacts);
        }
        if robot > 0.0 {
            self.robotize(robot);
        }
        // Real output: mirror the spectrum, then inverse FFT via conjugation.
        for k in 1..BINS - 1 {
            self.re[FRAME - k] = self.re[k];
            self.im[FRAME - k] = -self.im[k];
        }
        self.im.iter_mut().for_each(|v| *v = -*v);
        fft_in_place(&mut self.re, &mut self.im);
        let scale = 1.0 / FRAME as f32;
        for i in 0..FRAME {
            self.acc[i] += self.re[i] * scale * self.window[i];
        }
        self.ready.copy_from_slice(&self.acc[..HOP]);
        self.acc.copy_within(HOP.., 0);
        self.acc[HOP..].iter_mut().for_each(|v| *v = 0.0);
    }

    fn compress(&mut self, a: f32) {
        let bin_hz = self.sr / FRAME as f32;
        let flicker = 1.0 + 0.25 * a * (self.rng.next_f32() - 0.5);
        let cutoff_hz = (20_000.0 + (3_500.0 - 20_000.0) * a.powf(0.7)) * flicker;
        let cutoff = ((cutoff_hz / bin_hz) as usize).min(BINS);
        for k in cutoff..BINS {
            self.re[k] = 0.0;
            self.im[k] = 0.0;
        }
        let floor = 10f32.powf((-48.0 + 40.0 * a) / 10.0);
        let step_db = 1.0 + 5.0 * a;
        for i in 0..self.bands.len() {
            let (lo, hi) = self.bands[i];
            let hi = hi.min(cutoff);
            if lo >= hi {
                break;
            }
            let peak = (lo..hi).map(|k| self.re[k] * self.re[k] + self.im[k] * self.im[k]).fold(0.0f32, f32::max);
            if peak <= 0.0 {
                continue;
            }
            for k in lo..hi {
                let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
                let drop = p < peak * floor || (p < peak * 0.06 && self.rng.next_f32() < 0.5 * a);
                let g = if drop || p <= 0.0 {
                    0.0
                } else {
                    let db = 10.0 * p.log10();
                    10f32.powf(((db / step_db).round() * step_db - db) / 20.0)
                };
                self.re[k] *= g;
                self.im[k] *= g;
            }
        }
    }

    fn robotize(&mut self, r: f32) {
        for k in 0..BINS {
            let (re, im) = (self.re[k], self.im[k]);
            let mag = (re * re + im * im).sqrt();
            if mag == 0.0 {
                continue;
            }
            let phase = im.atan2(re);
            // A pulse in the middle of the frame (where the window is open): phase pi*k.
            let target = if k % 2 == 0 { 0.0 } else { PI };
            let mut d = target - phase;
            if d > PI {
                d -= 2.0 * PI;
            } else if d < -PI {
                d += 2.0 * PI;
            }
            let p = phase + r * d;
            self.re[k] = mag * p.cos();
            self.im[k] = mag * p.sin();
        }
    }
}
