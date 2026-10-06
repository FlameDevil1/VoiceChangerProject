//! Measurements used by tests (and later by the visualizer / auto-gain).

pub fn peak(x: &[f32]) -> f32 {
    crate::dsp::peak(x)
}

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

pub fn rms_db(x: &[f32]) -> f32 {
    crate::dsp::gain_to_db(rms(x))
}

/// Largest per-sample difference over the common length (lengths must match for a pass).
pub fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::INFINITY;
    }
    a.iter().zip(b).fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

/// Signal-to-error ratio of `test` against `reference`, in dB. Identical signals give +inf.
pub fn snr_db(reference: &[f32], test: &[f32]) -> f32 {
    let n = reference.len().min(test.len());
    let sig: f64 = reference[..n].iter().map(|s| (*s as f64).powi(2)).sum();
    let err: f64 = reference[..n].iter().zip(&test[..n]).map(|(r, t)| ((r - t) as f64).powi(2)).sum();
    if err == 0.0 {
        f32::INFINITY
    } else {
        (10.0 * (sig / err).log10()) as f32
    }
}

/// Fundamental frequency by the YIN cumulative-mean-normalised difference function, searched
/// between `fmin` and `fmax`. Returns `None` for unvoiced or silent input.
pub fn estimate_f0(x: &[f32], rate: u32, fmin: f32, fmax: f32) -> Option<f32> {
    let sr = rate as f32;
    let tau_min = (sr / fmax).floor() as usize;
    let tau_max = (sr / fmin).ceil() as usize;
    let w = x.len().checked_sub(tau_max + 2)?.min(4096);
    if w < tau_max || rms(&x[..w]) < 1e-4 {
        return None;
    }

    let diff = |tau: usize| -> f32 { (0..w).map(|i| (x[i] - x[i + tau]).powi(2)).sum() };
    let mut d = vec![0.0f32; tau_max + 2];
    let mut running = 0.0;
    for (tau, slot) in d.iter_mut().enumerate().skip(1) {
        let v = diff(tau);
        running += v;
        *slot = if running > 0.0 { v * tau as f32 / running } else { 1.0 };
    }

    // First dip below the threshold, then walk to its local minimum.
    const THRESHOLD: f32 = 0.15;
    let mut tau = (tau_min.max(2)..=tau_max).find(|&t| d[t] < THRESHOLD)?;
    while tau < tau_max && d[tau + 1] < d[tau] {
        tau += 1;
    }
    // Parabolic interpolation for sub-sample accuracy.
    let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
    let denom = a - 2.0 * b + c;
    let offset = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
    Some(sr / (tau as f32 + offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::signals;

    #[test]
    fn rms_of_sine() {
        let x = signals::sine(48_000, 1.0, 1000.0, 1.0);
        assert!((rms(&x) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn f0_of_sine_and_silence() {
        let x = signals::sine(48_000, 0.3, 233.0, 0.5);
        let f = estimate_f0(&x, 48_000, 60.0, 800.0).unwrap();
        assert!((f - 233.0).abs() < 0.5, "{f}");
        assert_eq!(estimate_f0(&signals::silence(48_000, 0.3), 48_000, 60.0, 800.0), None);
    }

    #[test]
    fn snr_and_diff() {
        let x = signals::sine(48_000, 0.1, 440.0, 0.5);
        assert_eq!(snr_db(&x, &x), f32::INFINITY);
        let y: Vec<f32> = x.iter().map(|s| s * 0.99).collect();
        assert!((snr_db(&x, &y) - 40.0).abs() < 0.1);
        assert!(max_abs_diff(&x, &x[1..]).is_infinite());
    }
}
