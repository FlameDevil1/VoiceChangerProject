//! Lookahead brickwall limiter, pinned at the end of the chain so the virtual mic never clips.
//!
//! Gain computer: required gain per sample -> sliding minimum over the lookahead window ->
//! instant attack / exponential release -> moving average over the same window. Averaging a
//! signal that is already at or below the target for the whole window guarantees the gain has
//! fully reached the target when the peak arrives (no overshoot), and turns the gain change into
//! a smooth ramp instead of a click. The audio is delayed by the window so the gain can lead it.

/// Lookahead in seconds (1 ms). Short enough to be negligible in the latency budget.
const LOOKAHEAD: f32 = 0.001;
const RELEASE: f32 = 0.080;

pub struct Limiter {
    ceiling: f32,
    window: usize,
    // Signal delay of window - 1 samples.
    delay: Vec<f32>,
    delay_pos: usize,
    // Monotonic deque for the sliding minimum: (gain, sample index).
    dq_val: Vec<f32>,
    dq_idx: Vec<u64>,
    dq_head: usize,
    dq_len: usize,
    release_coef: f32,
    released: f32,
    // Moving average of the released gain.
    avg_buf: Vec<f32>,
    avg_pos: usize,
    avg_sum: f64,
    n: u64,
}

impl Limiter {
    pub fn new(sample_rate: f32, ceiling_db: f32) -> Self {
        let window = ((sample_rate * LOOKAHEAD) as usize).max(2);
        Self {
            ceiling: crate::dsp::db_to_gain(ceiling_db),
            window,
            delay: vec![0.0; window - 1],
            delay_pos: 0,
            dq_val: vec![0.0; window + 1],
            dq_idx: vec![0; window + 1],
            dq_head: 0,
            dq_len: 0,
            release_coef: (-1.0 / (RELEASE * sample_rate)).exp(),
            released: 1.0,
            avg_buf: vec![1.0; window],
            avg_pos: 0,
            avg_sum: window as f64,
            n: 0,
        }
    }

    /// Delay added to the signal, in samples.
    pub fn latency(&self) -> usize {
        self.window - 1
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        let cap = self.dq_val.len();
        let w = self.window as u64;
        for s in buf.iter_mut() {
            // Never let NaN/inf from a buggy effect reach the virtual mic.
            let x = if s.is_finite() { *s } else { 0.0 };
            let a = x.abs();
            let g_req = if a > self.ceiling { self.ceiling / a } else { 1.0 };

            // Sliding minimum over the last `window` samples.
            while self.dq_len > 0 && self.dq_val[(self.dq_head + self.dq_len - 1) % cap] >= g_req {
                self.dq_len -= 1;
            }
            let tail = (self.dq_head + self.dq_len) % cap;
            self.dq_val[tail] = g_req;
            self.dq_idx[tail] = self.n;
            self.dq_len += 1;
            while self.dq_idx[self.dq_head] + w <= self.n {
                self.dq_head = (self.dq_head + 1) % cap;
                self.dq_len -= 1;
            }
            let hold = self.dq_val[self.dq_head];

            // Instant attack, smooth release (always stays at or below `hold`).
            self.released = if hold < self.released { hold } else { hold + (self.released - hold) * self.release_coef };

            self.avg_sum += (self.released - self.avg_buf[self.avg_pos]) as f64;
            self.avg_buf[self.avg_pos] = self.released;
            self.avg_pos = (self.avg_pos + 1) % self.window;
            let gain = (self.avg_sum / self.window as f64) as f32;

            let delayed = std::mem::replace(&mut self.delay[self.delay_pos], x);
            self.delay_pos = (self.delay_pos + 1) % self.delay.len();
            *s = (delayed * gain).clamp(-self.ceiling, self.ceiling);
            self.n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    fn limit(x: &[f32], block: usize) -> Vec<f32> {
        let mut l = Limiter::new(48_000.0, -1.0);
        let mut y = x.to_vec();
        for c in y.chunks_mut(block) {
            l.process(c);
        }
        y
    }

    #[test]
    fn quiet_signal_is_delayed_but_untouched() {
        let x = signals::vowel(48_000, 0.3, 150.0); // peak 0.5
        let y = limit(&x, 480);
        let d = Limiter::new(48_000.0, -1.0).latency();
        assert_eq!(&y[d..], &x[..x.len() - d]);
    }

    #[test]
    fn hot_signal_never_exceeds_ceiling() {
        let ceiling = crate::dsp::db_to_gain(-1.0);
        for amp in [1.0f32, 2.0, 8.0] {
            let mut x = signals::sine(48_000, 0.5, 220.0, amp);
            // Add a sudden transient.
            x[10_000] = amp * 1.5;
            let y = limit(&x, 480);
            assert!(analysis::peak(&y) <= ceiling + 1e-6, "amp {amp}: {}", analysis::peak(&y));
            // And it is limiting, not muting.
            assert!(analysis::peak(&y[12_000..]) > ceiling * 0.9);
        }
    }

    #[test]
    fn gain_changes_without_clicks_and_recovers() {
        let mut x = signals::sine(48_000, 0.3, 200.0, 0.3);
        x.extend(signals::sine(48_000, 0.3, 200.0, 3.0));
        x.extend(signals::sine(48_000, 0.6, 200.0, 0.3));
        let y = limit(&x, 64);
        // The 1 ms gain ramp adds no step bigger than the input's own (no click).
        let step = |v: &[f32]| v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(step(&y) < step(&x), "out {} vs in {}", step(&y), step(&x));
        // After release, the quiet tail is back to (nearly) unity gain.
        let tail = &y[y.len() - 4800..];
        assert!((analysis::peak(tail) - 0.3).abs() < 0.01, "{}", analysis::peak(tail));
    }

    #[test]
    fn nan_is_silenced() {
        let mut x = vec![0.1f32; 200];
        x[50] = f32::NAN;
        let y = limit(&x, 200);
        assert!(y.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn block_size_invariant() {
        let x = signals::sine(48_000, 0.2, 300.0, 2.0);
        assert_eq!(limit(&x, 1), limit(&x, 480));
    }
}
