//! Clock-drift compensation between the capture device and an output device.
//!
//! The mic, the headphones and the virtual cable each run on their own clock. Even at the same
//! nominal rate they drift apart, so a plain ring buffer slowly fills (latency grows) or drains
//! (periodic clicks). The output side reads the ring through a fractional resampler whose ratio is
//! nudged by a PI controller that holds the ring's fill level at a target. Corrections are capped
//! at ±0.3 % (about 5 cents), which is inaudible as pitch change.

use rtrb::Consumer;

/// Live statistics exposed to the UI (written by the audio thread).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DriftStats {
    /// Smoothed ring fill in engine samples.
    pub fill: f32,
    /// Current fill target in engine samples.
    pub target: f32,
    /// Current ratio correction (e.g. 0.0001 = +0.01 %).
    pub correction: f32,
}

pub struct DriftResampler {
    rx: Consumer<f32>,
    /// x[-1], x[0], x[1], x[2]: we interpolate between x[0] and x[1].
    hist: [f32; 4],
    mu: f64,
    /// engine rate / output rate.
    nominal: f64,
    correction: f64,
    integral: f64,
    fill_ema: f64,
    engine_rate: f64,
    out_rate: f64,
    /// Extra safety margin on top of the block-size-derived minimum, in seconds.
    margin: f64,
    max_out_block: usize,
    primed: bool,
    fade: f32,
    fade_step: f32,
    underruns: u32,
}

const MAX_CORRECTION: f64 = 0.003;
const KP: f64 = 0.4; // per second of fill error
const KI: f64 = 0.04;
const EMA_TAU: f64 = 0.5; // seconds
/// Fill this far above target means a hiccup dumped audio in the ring: drop the excess.
const RESYNC_EXCESS: f64 = 0.080; // seconds

impl DriftResampler {
    pub fn new(rx: Consumer<f32>, engine_rate: f64, out_rate: f64, margin_seconds: f64) -> Self {
        Self {
            rx,
            hist: [0.0; 4],
            mu: 0.0,
            nominal: engine_rate / out_rate,
            correction: 0.0,
            integral: 0.0,
            fill_ema: 0.0,
            engine_rate,
            out_rate,
            margin: margin_seconds,
            max_out_block: 0,
            primed: false,
            fade: 0.0,
            // 5 ms fade-in after (re)priming.
            fade_step: (1.0 / (0.005 * out_rate)) as f32,
            underruns: 0,
        }
    }

    pub fn set_margin(&mut self, seconds: f64) {
        self.margin = seconds;
    }

    pub fn margin(&self) -> f64 {
        self.margin
    }

    pub fn underruns(&self) -> u32 {
        self.underruns
    }

    /// Target fill in engine samples. `in_block` is the capture callback size in engine frames.
    ///
    /// The ring receives `in_block` samples at a time, so just before a write it can be up to one
    /// input block lower than average; it must still cover the output block we are about to read.
    fn target(&self, in_block: usize) -> f64 {
        // Until we have seen a steady-state output block, assume it matches the input period.
        let out_block = if self.max_out_block > 0 { self.max_out_block as f64 * self.nominal } else { in_block as f64 };
        in_block as f64 * 0.5 + out_block + self.margin * self.engine_rate
    }

    /// Fill `out` (mono, output rate). `in_block` = latest capture block size in engine frames.
    pub fn process(&mut self, out: &mut [f32], in_block: usize) -> DriftStats {
        // WASAPI's first callbacks ask for the whole device buffer; only count block sizes once
        // running, or that one-off request would inflate the latency target for good.
        if self.primed {
            self.max_out_block = self.max_out_block.max(out.len());
        }
        let target = self.target(in_block);
        let mut avail = self.rx.slots() as f64;
        let dt = out.len() as f64 / self.out_rate;

        if !self.primed {
            if avail < target {
                out.fill(0.0);
                return self.stats(target);
            }
            // Start exactly on target (output is silent and fading in, so dropping is inaudible)
            // instead of spending seconds steering there at maximum correction.
            let excess = (avail - target) as usize;
            if let Ok(chunk) = self.rx.read_chunk(excess) {
                chunk.commit_all();
            }
            self.primed = true;
            self.fade = 0.0;
            self.fill_ema = target;
            self.integral = 0.0;
            avail = target;
            self.hist = [0.0; 4];
            self.mu = 0.0;
        }

        // Smooth the fill level: it saw-tooths by one input block as writes arrive.
        let alpha = 1.0 - (-dt / EMA_TAU).exp();
        self.fill_ema += alpha * (avail - self.fill_ema);

        if avail - target > RESYNC_EXCESS * self.engine_rate {
            let drop = (avail - target) as usize;
            if let Ok(chunk) = self.rx.read_chunk(drop) {
                chunk.commit_all();
            }
            self.fill_ema = target;
            self.integral = 0.0;
        }

        // PI controller on fill error (seconds). Too full -> read faster (ratio up).
        let err = (self.fill_ema - target) / self.engine_rate;
        self.integral = (self.integral + err * dt).clamp(-MAX_CORRECTION / KI, MAX_CORRECTION / KI);
        self.correction = (KP * err + KI * self.integral).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        let ratio = self.nominal * (1.0 + self.correction);

        for (i, o) in out.iter_mut().enumerate() {
            while self.mu >= 1.0 {
                match self.rx.pop() {
                    Ok(s) => {
                        self.hist = [self.hist[1], self.hist[2], self.hist[3], s];
                        self.mu -= 1.0;
                    }
                    Err(_) => {
                        // Underrun: go silent and re-prime. Grow the margin so it doesn't recur.
                        self.underruns += 1;
                        self.primed = false;
                        self.margin = (self.margin + 0.001).min(0.050);
                        out[i..].fill(0.0);
                        return self.stats(target);
                    }
                }
            }
            let y = hermite(&self.hist, self.mu as f32);
            if self.fade < 1.0 {
                self.fade = (self.fade + self.fade_step).min(1.0);
                *o = y * self.fade;
            } else {
                *o = y;
            }
            self.mu += ratio;
        }
        self.stats(target)
    }

    fn stats(&self, target: f64) -> DriftStats {
        DriftStats { fill: self.fill_ema as f32, target: target as f32, correction: self.correction as f32 }
    }
}

/// 4-point cubic Hermite (Catmull-Rom) interpolation between h[1] and h[2].
///
/// Plenty for drift correction where the ratio stays within a fraction of a percent of 1.0.
/// Large rate conversions (e.g. 44.1k -> 48k) are left to the Windows audio engine.
#[inline]
fn hermite(h: &[f32; 4], t: f32) -> f32 {
    let [xm1, x0, x1, x2] = *h;
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    #[test]
    fn hermite_reconstructs_sine() {
        let f = 0.01f32; // cycles per sample (480 Hz at 48 kHz)
        let s = |n: f32| (std::f32::consts::TAU * f * n).sin();
        for k in 0..100 {
            let n = k as f32;
            let h = [s(n - 1.0), s(n), s(n + 1.0), s(n + 2.0)];
            for t in [0.25f32, 0.5, 0.75] {
                assert!((hermite(&h, t) - s(n + t)).abs() < 1e-3);
            }
        }
    }

    /// Simulate a capture clock and an output clock that disagree by `drift`, with different
    /// block sizes, and check the controller keeps the ring near target with no underruns.
    fn simulate(drift: f64, in_block: usize, out_block: usize, seconds: f64) -> (DriftResampler, Vec<f32>) {
        let sr = 48_000.0;
        let (mut tx, rx) = RingBuffer::<f32>::new(sr as usize);
        let mut rs = DriftResampler::new(rx, sr, sr, 0.005);
        let mut out = vec![0.0f32; out_block];
        let in_period = in_block as f64 / sr;
        let out_period = out_block as f64 / (sr * (1.0 + drift));
        let (mut t_in, mut t_out) = (0.0f64, 0.0f64);
        let mut phase = 0u64;
        let mut fills = Vec::new();
        while t_in.min(t_out) < seconds {
            if t_in <= t_out {
                for _ in 0..in_block {
                    let _ = tx.push((phase as f32 * 0.001).sin());
                    phase += 1;
                }
                t_in += in_period;
            } else {
                let st = rs.process(&mut out, in_block);
                fills.push(st.fill - st.target);
                t_out += out_period;
            }
        }
        (rs, fills)
    }

    #[test]
    fn tracks_drift_without_underruns() {
        // 0.05 % drift is far worse than real hardware (typically < 0.01 %).
        for drift in [-0.0005, 0.0, 0.0005] {
            let (rs, fills) = simulate(drift, 480, 441, 120.0);
            assert_eq!(rs.underruns(), 0, "drift {drift}");
            // After settling, fill stays within 3 ms of target.
            let tail = &fills[fills.len() / 2..];
            let worst = tail.iter().fold(0.0f32, |m, e| m.max(e.abs()));
            assert!(worst < 0.003 * 48_000.0, "drift {drift}: worst fill error {worst}");
        }
    }

    #[test]
    fn underrun_grows_margin_and_recovers() {
        let sr = 48_000.0;
        let (mut tx, rx) = RingBuffer::<f32>::new(4800);
        let mut rs = DriftResampler::new(rx, sr, sr, 0.001);
        let mut out = vec![0.0f32; 480];
        for _ in 0..2000 {
            let _ = tx.push(0.1);
        }
        rs.process(&mut out, 480);
        // Starve it.
        for _ in 0..10 {
            rs.process(&mut out, 480);
        }
        assert!(rs.underruns() >= 1);
        assert!(rs.margin() > 0.001);
        assert!(out.iter().all(|s| *s == 0.0), "silent while starved");
    }
}
