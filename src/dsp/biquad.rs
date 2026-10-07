//! RBJ "Audio EQ Cookbook" biquads, transposed direct form II.

use std::f32::consts::PI;

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    LowPass,
    HighPass,
    Peak,
    LowShelf,
    HighShelf,
}

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, z1: 0.0, z2: 0.0 }
    }
}

impl Biquad {
    /// Update coefficients, keeping the filter state (so parameter changes don't click).
    pub fn set(&mut self, shape: Shape, sample_rate: f32, freq: f32, q: f32, gain_db: f32) {
        let f = freq.clamp(10.0, sample_rate * 0.49);
        let w = 2.0 * PI * f / sample_rate;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / (2.0 * q.max(0.05));
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::LowPass => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            Shape::HighPass => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            Shape::Peak => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
            Shape::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                    a * ((a + 1.0) - (a - 1.0) * cw - s),
                    (a + 1.0) + (a - 1.0) * cw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                    (a + 1.0) + (a - 1.0) * cw - s,
                )
            }
            Shape::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                    a * ((a + 1.0) + (a - 1.0) * cw - s),
                    (a + 1.0) - (a - 1.0) * cw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cw),
                    (a + 1.0) - (a - 1.0) * cw - s,
                )
            }
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Magnitude response in dB at `freq` (for tests and a future EQ curve display).
    pub fn response_db(&self, sample_rate: f32, freq: f32) -> f32 {
        let w = 2.0 * PI * freq / sample_rate;
        let (c1, s1, c2, s2) = (w.cos(), -w.sin(), (2.0 * w).cos(), -(2.0 * w).sin());
        let (nr, ni) = (self.b0 + self.b1 * c1 + self.b2 * c2, self.b1 * s1 + self.b2 * s2);
        let (dr, di) = (1.0 + self.a1 * c1 + self.a2 * c2, self.a1 * s1 + self.a2 * s2);
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_have_expected_response() {
        let sr = 48_000.0;
        let mut b = Biquad::default();
        b.set(Shape::Peak, sr, 1000.0, 1.0, 6.0);
        assert!((b.response_db(sr, 1000.0) - 6.0).abs() < 0.01);
        assert!(b.response_db(sr, 50.0).abs() < 0.1);
        b.set(Shape::LowShelf, sr, 120.0, 0.707, -9.0);
        assert!((b.response_db(sr, 20.0) + 9.0).abs() < 0.3);
        assert!(b.response_db(sr, 5000.0).abs() < 0.1);
        b.set(Shape::HighPass, sr, 300.0, 0.707, 0.0);
        assert!((b.response_db(sr, 300.0) + 3.0).abs() < 0.1);
        assert!(b.response_db(sr, 30.0) < -35.0);
        b.set(Shape::LowPass, sr, 3400.0, 0.707, 0.0);
        assert!(b.response_db(sr, 12_000.0) < -20.0);
    }
}
