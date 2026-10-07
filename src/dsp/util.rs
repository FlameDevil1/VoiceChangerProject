/// Convert decibels to linear gain.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Convert linear gain to decibels, floored at -120 dB.
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
}

/// Flush denormals to zero on the calling thread (x86 MXCSR FTZ + DAZ).
///
/// Decaying signals (filter and reverb tails) produce denormal floats, which are up to ~100x
/// slower on x86. Called at the top of every audio callback and offline render, so live and file
/// output stay bit-identical.
#[inline]
pub fn enable_ftz() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040; // FTZ (bit 15) | DAZ (bit 6)
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, readonly));
    }
}

/// Fixed delay line (allocated up front, real-time safe).
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    pub fn new(delay: usize) -> Self {
        Self { buf: vec![0.0; delay], pos: 0 }
    }

    pub fn delay(&self) -> usize {
        self.buf.len()
    }

    /// Push one sample, return the sample from `delay` samples ago.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if self.buf.is_empty() {
            return x;
        }
        let y = std::mem::replace(&mut self.buf[self.pos], x);
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        y
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }
}

/// Raised-cosine (Hann) window lookup: `w(u) = 0.5 (1 + cos(pi u))` for |u| < 1, else 0.
/// A table with linear interpolation is ~10x cheaper than `cos` per sample, error < 1e-6.
#[derive(Clone, Debug)]
pub struct HannTable {
    table: Vec<f32>,
}

impl HannTable {
    const SIZE: usize = 1024;

    pub fn new() -> Self {
        let table = (0..=Self::SIZE + 1)
            .map(|i| {
                let u = (i as f64 / Self::SIZE as f64).min(1.0);
                (0.5 * (1.0 + (std::f64::consts::PI * u).cos())) as f32
            })
            .collect();
        Self { table }
    }

    #[inline]
    pub fn at(&self, u: f32) -> f32 {
        let x = u.abs() * Self::SIZE as f32;
        if x >= Self::SIZE as f32 {
            return 0.0;
        }
        let i = x as usize;
        let f = x - i as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }
}

impl Default for HannTable {
    fn default() -> Self {
        Self::new()
    }
}

/// A parameter that ramps linearly to its target over a fixed time, to avoid zipper noise
/// and clicks when sliders or toggles change.
#[derive(Clone, Debug)]
pub struct SmoothedValue {
    current: f32,
    target: f32,
    step: f32,
    remaining: u32,
    ramp_samples: u32,
}

impl SmoothedValue {
    pub fn new(initial: f32, sample_rate: f32, ramp_seconds: f32) -> Self {
        Self {
            current: initial,
            target: initial,
            step: 0.0,
            remaining: 0,
            ramp_samples: ((sample_rate * ramp_seconds) as u32).max(1),
        }
    }

    pub fn set_target(&mut self, target: f32) {
        if target != self.target {
            self.target = target;
            self.remaining = self.ramp_samples;
            self.step = (target - self.current) / self.ramp_samples as f32;
        }
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    /// Jump to the target immediately (no ramp).
    pub fn snap(&mut self) {
        self.current = self.target;
        self.remaining = 0;
    }

    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }

    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.current = if self.remaining == 0 { self.target } else { self.current + self.step };
        }
        self.current
    }

    /// Advance `n` samples without producing output.
    pub fn skip(&mut self, n: usize) {
        let n = n.min(self.remaining as usize) as u32;
        self.remaining -= n;
        self.current = if self.remaining == 0 { self.target } else { self.current + self.step * n as f32 };
    }

    /// Multiply `buf` by the (ramping) value.
    pub fn apply(&mut self, buf: &mut [f32]) {
        if self.is_settled() {
            if self.current != 1.0 {
                buf.iter_mut().for_each(|s| *s *= self.current);
            }
        } else {
            buf.iter_mut().for_each(|s| *s *= self.next_value());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip() {
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((gain_to_db(0.5) + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn smoothed_reaches_target_exactly() {
        let mut v = SmoothedValue::new(0.0, 1000.0, 0.01); // 10 samples
        v.set_target(1.0);
        let vals: Vec<f32> = (0..12).map(|_| v.next_value()).collect();
        assert!(vals.windows(2).all(|w| w[1] >= w[0]), "monotonic");
        assert_eq!(vals[9], 1.0);
        assert!(v.is_settled());
    }

    #[test]
    fn skip_matches_next() {
        let mut a = SmoothedValue::new(0.0, 1000.0, 0.01);
        let mut b = a.clone();
        a.set_target(2.0);
        b.set_target(2.0);
        for _ in 0..4 {
            a.next_value();
        }
        b.skip(4);
        assert!((a.current() - b.current()).abs() < 1e-6);
        b.skip(100);
        assert_eq!(b.current(), 2.0);
    }
}
