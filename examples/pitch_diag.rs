//! Diagnostics for the pitch shifter: pitch accuracy and spectral shape vs a chipmunk reference.
use voice_changer::dsp::{CoreParams, FxSettings};
use voice_changer::offline::{self, analysis, signals};

/// Centroid restricted to [lo, hi] Hz (excludes the fundamental region).
fn band_centroid(x: &[f32], rate: u32, lo: f64, hi: f64) -> f64 {
    let n = x.len().min(4096);
    let s = (x.len() - n) / 2;
    let seg: Vec<f64> = x[s..s + n].iter().enumerate()
        .map(|(i, v)| *v as f64 * 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / n as f64).cos())).collect();
    let (mut num, mut den) = (0.0, 0.0);
    for k in 1..n / 2 {
        let f = k as f64 * rate as f64 / n as f64;
        if f < lo || f > hi { continue; }
        let w = std::f64::consts::TAU * k as f64 / n as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in seg.iter().enumerate() { re += v * (w * i as f64).cos(); im += v * (w * i as f64).sin(); }
        let p = re * re + im * im; num += p * f; den += p;
    }
    num / den
}

fn main() {
    let x = signals::vowel(48_000, 0.8, 140.0);
    let st = |y: &[f32]| y[9600..y.len() - 2400].to_vec();
    let xs = st(&x);
    let full0 = analysis::spectral_centroid(&xs, 48_000) as f64;
    let band0 = band_centroid(&xs, 48_000, 400.0, 4000.0);
    println!("{:>6} {:>8} {:>8} {:>8} {:>8}", "shift", "f0", "full", "band", "rms");
    for (semi, fm) in [(-12.0f32, 0.0f32), (-5.0, 0.0), (4.0, 0.0), (7.0, 0.0), (12.0, 0.0), (0.0, 4.0), (0.0, -4.0), (7.0, 7.0)] {
        let mut fx = FxSettings::default();
        fx.pitch.enabled = true; fx.pitch.semitones = semi; fx.pitch.formant = fm;
        let y = st(&offline::render(&x, 48_000, CoreParams::default(), &fx, 480));
        println!("{:>3}/{:>2} {:>8.1} {:>8.2} {:>8.2} {:>8.2}", semi, fm,
            analysis::estimate_f0(&y, 48_000, 50.0, 800.0).unwrap_or(0.0),
            analysis::spectral_centroid(&y, 48_000) as f64 / full0,
            band_centroid(&y, 48_000, 400.0, 4000.0) / band0,
            analysis::rms(&y) / analysis::rms(&xs));
    }
}
