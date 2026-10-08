//! Loudness check: how much each effect preset and built-in voice changes the level of a
//! speech-like signal. Big jumps are jarring when you click through presets.
//!
//! `cargo run --release --example preset_levels`
use voice_changer::dsp::{CoreParams, EffectKind, FxSettings};
use voice_changer::offline::{self, analysis, signals};
use voice_changer::presets;

fn speech() -> Vec<f32> {
    let mut x = signals::vowel_wobble(48_000, 1.5, 140.0, 0.15, 2.0);
    x.extend(signals::silence(48_000, 0.3));
    x.extend(signals::vowel_wobble(48_000, 1.0, 180.0, 0.1, 3.0).iter().map(|s| s * 0.5));
    x
}

fn report(name: &str, x: &[f32], fx: &FxSettings) {
    let y = offline::render(x, 48_000, CoreParams::default(), fx, 480);
    let d = analysis::rms_db(&y) - analysis::rms_db(x);
    let flag = if d.abs() > 4.0 { "  <-- level jump" } else { "" };
    println!("{name:<34} {d:+6.1} dB   peak {:.2}{flag}", analysis::peak(&y));
}

fn main() {
    let x = speech();
    println!("input: {:.1} dBFS rms, peak {:.2}\n", analysis::rms_db(&x), analysis::peak(&x));
    println!("-- effect presets --");
    for kind in EffectKind::ALL {
        for (name, values) in kind.spec().presets {
            let fx = FxSettings::default().with(kind, values);
            report(&format!("{} / {name}", kind.key()), &x, &fx);
        }
    }
    println!("\n-- built-in voices --");
    for p in presets::builtins() {
        report(&p.name, &x, &p.fx);
    }
    println!("\n-- scenarios --");
    for s in presets::SCENARIOS {
        report(s.name, &x, &s.apply(&FxSettings::default()));
    }
}
