//! Deterministic test signals. No recordings are needed in the repo: every test input is
//! generated here, so goldens are reproducible bit for bit.

use std::f64::consts::TAU;

/// Small, fast, seeded PRNG (xorshift64*). Effects with randomness (bad-connection, crackle)
/// must use a seeded generator like this so presets and tests are reproducible.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn len(rate: u32, secs: f64) -> usize {
    (rate as f64 * secs).round() as usize
}

pub fn silence(rate: u32, secs: f64) -> Vec<f32> {
    vec![0.0; len(rate, secs)]
}

pub fn sine(rate: u32, secs: f64, freq: f64, amp: f32) -> Vec<f32> {
    (0..len(rate, secs)).map(|n| amp * (TAU * freq * n as f64 / rate as f64).sin() as f32).collect()
}

/// Logarithmic sweep from `f0` to `f1` Hz at amplitude 0.5.
pub fn sweep(rate: u32, secs: f64, f0: f64, f1: f64) -> Vec<f32> {
    let n = len(rate, secs);
    let k = (f1 / f0).ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / rate as f64;
            let phase = TAU * f0 * secs / k * ((t / secs * k).exp() - 1.0);
            0.5 * phase.sin() as f32
        })
        .collect()
}

/// White noise in [-amp, amp].
pub fn noise(rate: u32, secs: f64, amp: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..len(rate, secs)).map(|_| amp * (2.0 * rng.next_f32() - 1.0)).collect()
}

/// A synthetic sustained "ah" vowel: a band-limited glottal source at `f0` through a Klatt-style
/// cascade of three formant resonators (F1 700 Hz, F2 1220 Hz, F3 2600 Hz). Pitch and formants are known
/// exactly, which is what pitch- and formant-shift tests need, and it sounds voice-like enough to
/// judge effects by ear.
pub fn vowel(rate: u32, secs: f64, f0: f64) -> Vec<f32> {
    let n = len(rate, secs);
    let sr = rate as f64;
    // Glottal source: one period of harmonics with -6 dB/octave rolloff (glottal -12 dB plus
    // +6 dB lip radiation), band-limited below
    // Nyquist, stored as a wavetable and played back with a phase accumulator (cheap enough for
    // minute-long test signals).
    const TABLE: usize = 4096;
    let harmonics = ((sr * 0.45) / f0) as usize;
    let table: Vec<f64> = (0..=TABLE)
        .map(|i| {
            let ph = TAU * i as f64 / TABLE as f64;
            (1..=harmonics).map(|h| (ph * h as f64).sin() / h as f64).sum()
        })
        .collect();
    let inc = f0 / sr * TABLE as f64;
    let mut phase = 0.0f64;
    let mut src: Vec<f64> = (0..n)
        .map(|_| {
            let i = phase as usize;
            let f = phase - i as f64;
            let v = table[i] + (table[i + 1] - table[i]) * f;
            phase += inc;
            if phase >= TABLE as f64 {
                phase -= TABLE as f64;
            }
            v
        })
        .collect();

    for (fc, bw) in [(700.0, 110.0), (1220.0, 120.0), (2600.0, 160.0)] {
        // Two-pole resonator with unity gain at 0 Hz, so the cascade keeps the low harmonics and
        // each formant stands out as a peak.
        let r = (-std::f64::consts::PI * bw / sr).exp();
        let a1 = -2.0 * r * (TAU * fc / sr).cos();
        let a2 = r * r;
        let g = 1.0 + a1 + a2;
        let (mut y1, mut y2) = (0.0, 0.0);
        for s in src.iter_mut() {
            let y = g * *s - a1 * y1 - a2 * y2;
            y2 = y1;
            y1 = y;
            *s = y;
        }
    }

    let peak = src.iter().fold(0.0f64, |m, s| m.max(s.abs())).max(1e-9);
    // 20 ms fade in/out so the signal starts and ends without clicks.
    let fade = len(rate, 0.02).max(1);
    src.iter()
        .enumerate()
        .map(|(i, s)| {
            let env = (i.min(n - 1 - i) as f64 / fade as f64).min(1.0);
            (0.5 * s / peak * env) as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::analysis;

    #[test]
    fn signals_are_deterministic_and_bounded() {
        assert_eq!(noise(48_000, 0.1, 0.5, 7), noise(48_000, 0.1, 0.5, 7));
        assert_ne!(noise(48_000, 0.1, 0.5, 7), noise(48_000, 0.1, 0.5, 8));
        for x in [vowel(48_000, 0.3, 120.0), sweep(48_000, 0.3, 50.0, 18_000.0), sine(48_000, 0.3, 440.0, 0.5)] {
            assert!(analysis::peak(&x) <= 0.5 + 1e-6);
            assert!(analysis::peak(&x) > 0.4);
        }
    }

    #[test]
    fn vowel_has_requested_pitch() {
        for f0 in [100.0, 140.0, 220.0] {
            let x = vowel(48_000, 0.5, f0);
            let est = analysis::estimate_f0(&x[4800..], 48_000, 60.0, 500.0).unwrap();
            assert!((est - f0 as f32).abs() / (f0 as f32) < 0.01, "f0 {f0}: estimated {est}");
        }
    }
}
