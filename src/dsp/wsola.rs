//! WSOLA time-stretching reader: plays recorded audio at a different speed without changing
//! its pitch. Used live by the bad connection effect (falling behind / catching up) and offline
//! for the file speed control.

use super::util::HannTable;

#[derive(Clone, Copy, Debug)]
struct Grain {
    src: i64,
    t: usize,
    on: bool,
}

/// Time-stretching reader over the history (WSOLA): overlapping 20 ms Hann grains every 10 ms.
/// At rate 1 each grain continues the previous one exactly, reproducing the input delayed. At
/// other rates each grain's start is nudged (within 5 ms) to line up with the waveform the
/// previous grain would have continued with, so speed changes don't change pitch.
pub(crate) struct Wsola {
    len: usize,
    pub(crate) hop: usize,
    search: i64,
    grains: [Grain; 2],
    newest: usize,
    started: bool,
    target: f64,
    next_in: usize,
    pub(crate) rate: f64,
}

impl Wsola {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let hop = ((sample_rate * 0.010) as usize).max(2);
        Self {
            len: 2 * hop,
            hop,
            search: (sample_rate * 0.005) as i64,
            grains: [Grain { src: 0, t: 0, on: false }; 2],
            newest: 0,
            started: false,
            target: 0.0,
            next_in: 0,
            rate: 1.0,
        }
    }

    /// Smallest delay that leaves room for the alignment search.
    pub(crate) fn min_delay(&self) -> f64 {
        (self.hop as i64 + self.search + 2) as f64
    }

    pub(crate) fn start(&mut self, from: i64, rate: f64) {
        self.grains = [Grain { src: 0, t: 0, on: false }; 2];
        self.started = false;
        self.target = from as f64;
        self.next_in = 0;
        self.rate = rate;
    }

    /// Input position being played (for the delay readout).
    pub(crate) fn position(&self) -> f64 {
        let g = self.grains[self.newest];
        g.src as f64 + g.t as f64
    }

    pub(crate) fn next(&mut self, h: &[f32], mask: usize, written: i64, hann: &HannTable) -> f32 {
        if self.next_in == 0 {
            self.spawn(h, mask, written);
            self.next_in = self.hop;
        }
        self.next_in -= 1;
        let mut y = 0.0;
        for g in &mut self.grains {
            if g.on {
                let u = 1.0 - 2.0 * g.t as f32 / self.len as f32;
                y += hann.at(u) * h[(g.src + g.t as i64) as usize & mask];
                g.t += 1;
                if g.t >= self.len {
                    g.on = false;
                }
            }
        }
        y
    }

    fn spawn(&mut self, h: &[f32], mask: usize, written: i64) {
        let hop = self.hop as i64;
        let mut src = if !self.started {
            self.started = true;
            self.target.round() as i64
        } else {
            let natural = self.grains[self.newest].src + hop;
            if self.rate == 1.0 {
                self.target = natural as f64;
                natural
            } else {
                self.target += hop as f64 * self.rate;
                let center = self.target.round() as i64;
                // Compare only audio that has been recorded already.
                let last = written - hop;
                let (lo, hi) = (center - self.search, (center + self.search).min(last));
                if natural <= last && lo <= hi {
                    let at = |i: i64| h[i as usize & mask];
                    let (mut best, mut best_k) = (f32::MIN, center.min(last));
                    let mut k = lo;
                    while k <= hi {
                        let (mut dot, mut energy) = (0.0f32, 1e-9f32);
                        let mut i = 0;
                        while i < hop {
                            let a = at(k + i);
                            dot += a * at(natural + i);
                            energy += a * a;
                            i += 2;
                        }
                        let score = dot / energy.sqrt();
                        if score > best {
                            (best, best_k) = (score, k);
                        }
                        k += 2;
                    }
                    best_k
                } else {
                    center.min(last)
                }
            }
        };
        // A grain reads one new sample per output sample, so it may start at the newest one.
        src = src.min(written - 1);
        let slot = 1 - self.newest;
        self.grains[slot] = Grain { src, t: 0, on: true };
        self.newest = slot;
    }
}
