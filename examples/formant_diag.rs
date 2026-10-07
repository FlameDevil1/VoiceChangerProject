//! Formant diagnostics: spectral envelope peaks (F1/F2) before and after shifting.
use voice_changer::dsp::{CoreParams, FxSettings};
use voice_changer::offline::{self, signals};

fn harm_db(x: &[f32], f: f64) -> f64 {
    let n = x.len();
    let w = std::f64::consts::TAU * f / 48_000.0;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let h = 0.5 * (1.0 - (std::f64::consts::TAU * i as f64 / n as f64).cos());
        re += *v as f64 * h * (w * i as f64).cos();
        im += *v as f64 * h * (w * i as f64).sin();
    }
    20.0 * ((re * re + im * im).sqrt() / n as f64 * 4.0).max(1e-9).log10()
}

/// Frequency of the strongest harmonic of `f0` within [lo, hi].
fn peak(x: &[f32], f0: f64, lo: f64, hi: f64) -> f64 {
    let mut best = (0.0, f64::MIN);
    let mut f = (lo / f0).ceil() * f0;
    while f <= hi {
        let d = harm_db(x, f);
        if d > best.1 { best = (f, d); }
        f += f0;
    }
    best.0
}

fn main() {
    let f0 = 100.0;
    let x = signals::vowel(48_000, 0.8, f0);
    let seg = |y: &[f32]| y[19200..19200 + 8192].to_vec();
    println!("{:>8} {:>6} {:>6} {:>6}   (expected F1 700*r, F2 1220*r, F3 2600*r)", "formant", "F1", "F2", "F3");
    for fm in [0.0f32, -4.0, 4.0, -8.0, 8.0] {
        let mut fx = FxSettings::default();
        fx.pitch.enabled = true;
        fx.pitch.formant = fm;
        let y = seg(&offline::render(&x, 48_000, CoreParams::default(), &fx, 480));
        let r = 2f64.powf(fm as f64 / 12.0);
        println!("{:>5} r{:.2} {:>6.0} {:>6.0} {:>6.0}   (exp {:.0} {:.0} {:.0})", fm, r,
            peak(&y, f0, 300.0, 1000.0 * r.max(1.0)), peak(&y, f0, 900.0 * r, 1700.0 * r), peak(&y, f0, 2000.0 * r, 3200.0 * r),
            700.0 * r, 1220.0 * r, 2600.0 * r);
    }
}
