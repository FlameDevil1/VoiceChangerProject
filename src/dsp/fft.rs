//! Small radix-2 FFT for the spectrum display (UI thread only; allocates).

use std::f32::consts::PI;

/// Magnitudes of the first n/2 bins of a Hann-windowed real signal. `x.len()` must be a power
/// of two. Magnitudes are scaled so a full-scale sine reads about 1.0.
pub fn spectrum(x: &[f32]) -> Vec<f32> {
    let n = x.len();
    assert!(n.is_power_of_two() && n >= 2, "FFT size must be a power of two");
    let mut re: Vec<f32> =
        x.iter().enumerate().map(|(i, s)| s * 0.5 * (1.0 - (2.0 * PI * i as f32 / n as f32).cos())).collect();
    let mut im = vec![0.0f32; n];
    fft_in_place(&mut re, &mut im);
    // Hann window has coherent gain 0.5; a sine's energy is split over +/- frequencies.
    let scale = 4.0 / n as f32;
    (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt() * scale).collect()
}

/// In-place radix-2 FFT (no allocation). `re.len()` must be a power of two.
pub(crate) fn fft_in_place(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // Bit-reversal permutation.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_direct_dft() {
        let n = 64;
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 13) as f32 - 6.0) / 6.0).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        fft_in_place(&mut re, &mut im);
        for k in 0..n {
            let (mut dr, mut di) = (0.0f64, 0.0f64);
            for (t, v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                dr += *v as f64 * a.cos();
                di += *v as f64 * a.sin();
            }
            assert!((re[k] as f64 - dr).abs() < 1e-3 && (im[k] as f64 - di).abs() < 1e-3, "bin {k}");
        }
    }

    #[test]
    fn sine_peaks_at_its_bin_with_unit_magnitude() {
        let n = 2048;
        let bin = 100;
        let x: Vec<f32> = (0..n).map(|i| (2.0 * PI * bin as f32 * i as f32 / n as f32).sin()).collect();
        let s = spectrum(&x);
        let peak = s.iter().enumerate().fold((0, 0.0f32), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
        assert_eq!(peak.0, bin);
        assert!((peak.1 - 1.0).abs() < 0.02, "{}", peak.1);
    }
}
