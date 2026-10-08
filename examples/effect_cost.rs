//! CPU cost of each effect, offline: share of one core needed to keep up with real time.
//! Each effect runs with its first preset (or its defaults) on 20 s of speech-like audio in
//! 480-sample blocks. Live numbers are a little higher (cache misses, other threads).
//!
//! `cargo run --release --example effect_cost`
use std::time::Instant;
use voice_changer::dsp::{CoreParams, EffectKind, FxSettings};
use voice_changer::offline::{self, signals};

fn cost(x: &[f32], fx: &FxSettings) -> f64 {
    // Best of three, to skip warm-up and noise from other processes.
    (0..3)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(offline::render(x, 48_000, CoreParams::default(), fx, 480));
            t.elapsed().as_secs_f64()
        })
        .fold(f64::MAX, f64::min)
        / (x.len() as f64 / 48_000.0)
        * 100.0
}

fn main() {
    let x = signals::vowel_wobble(48_000, 20.0, 150.0, 0.15, 2.0);
    let base = cost(&x, &FxSettings::default());
    println!("{:<20} {:>7}", "effect", "% core");
    println!("{:<20} {base:>6.2}%", "(engine, no effects)");
    let mut all = FxSettings::default();
    for kind in EffectKind::ALL {
        let values = kind.spec().presets.first().map_or(&[][..], |(_, v)| *v);
        all = all.with(kind, values);
        let c = cost(&x, &FxSettings::default().with(kind, values)) - base;
        println!("{:<20} {c:>6.2}%", kind.label());
    }
    println!("{:<20} {:>6.2}%", "all together", cost(&x, &all) - base);
}
