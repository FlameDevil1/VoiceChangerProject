/// Convert decibels to linear gain.
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Convert linear gain to decibels, floored at -120 dB.
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.max(1e-6).log10()
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
