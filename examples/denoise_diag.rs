//! How much RNNoise reduces different noises over time (per 0.5 s window).
use voice_changer::dsp::{Chain, EffectKind, FxParams, FxSettings};
use voice_changer::offline::{analysis, signals};

fn main() {
    let rate = 48_000u32;
    let fx = FxParams::from_settings(&FxSettings::default().with(EffectKind::Denoise, &[]));
    let mut brown = Vec::new();
    let mut acc = 0.0f32;
    for s in signals::noise(rate, 6.0, 1.0, 5) {
        acc = acc * 0.995 + s * 0.05;
        brown.push(acc);
    }
    let cases: Vec<(&str, Vec<f32>)> = vec![
        ("white 0.01", signals::noise(rate, 6.0, 0.01, 1)),
        ("white 0.05", signals::noise(rate, 6.0, 0.05, 2)),
        ("white 0.2", signals::noise(rate, 6.0, 0.2, 3)),
        ("brown", brown),
        (
            "hum 50Hz+harm",
            (0..rate as usize * 6)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    0.05 * ((std::f32::consts::TAU * 50.0 * t).sin() + 0.5 * (std::f32::consts::TAU * 150.0 * t).sin())
                })
                .collect(),
        ),
    ];
    for (name, x) in cases {
        let mut chain = Chain::build(&[EffectKind::Denoise], &fx, rate as f32, 480);
        let mut y = x.clone();
        y.chunks_mut(480).for_each(|c| chain.process(c));
        let red: Vec<String> = x
            .chunks(24_000)
            .zip(y.chunks(24_000))
            .map(|(a, b)| format!("{:5.1}", analysis::rms_db(a) - analysis::rms_db(b)))
            .collect();
        println!("{name:>14}: in {:6.1} dBFS | reduction per 0.5 s: {}", analysis::rms_db(&x), red.join(" "));
    }
}
