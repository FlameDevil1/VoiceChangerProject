//! Behavioural tests for every effect, run through the real chain (slot + processor).
//!
//! Generic checks apply to all effects (so a new effect is covered automatically); specific
//! checks verify each effect does what its controls say.

use voice_changer::dsp::{Chain, EffectKind, FxParams, FxSettings};
use voice_changer::offline::{analysis, signals};

const RATE: u32 = 48_000;

/// Run `x` through a chain containing only `kind`, configured by `fx`.
fn run(kind: EffectKind, fx: &FxSettings, x: &[f32], block: usize) -> Vec<f32> {
    let params = FxParams::from_settings(fx);
    let mut chain = Chain::build(&[kind], &params, RATE as f32, block.max(480));
    let mut y = x.to_vec();
    for c in y.chunks_mut(block) {
        chain.process(c);
    }
    y
}

fn on(kind: EffectKind, values: &[(&str, f32)]) -> FxSettings {
    FxSettings::default().with(kind, values)
}

fn level_db(x: &[f32]) -> f32 {
    analysis::rms_db(x)
}

fn max_step(x: &[f32]) -> f32 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

// ---- generic --------------------------------------------------------------------------------

#[test]
fn every_effect_and_preset_is_finite_bounded_and_block_size_invariant() {
    let mut x = signals::vowel(RATE, 0.4, 160.0);
    x.extend(signals::noise(RATE, 0.2, 0.2, 5));
    x.extend(signals::sine(RATE, 0.2, 440.0, 0.99));
    for kind in EffectKind::ALL {
        let mut configs = vec![("defaults", on(kind, &[]))];
        for (name, values) in kind.spec().presets {
            configs.push((name, on(kind, values)));
        }
        for (name, fx) in configs {
            let reference = run(kind, &fx, &x, 480);
            assert!(reference.iter().all(|s| s.is_finite()), "{kind:?}/{name}: non-finite output");
            assert!(analysis::peak(&reference) < 4.0, "{kind:?}/{name}: peak {}", analysis::peak(&reference));
            for block in [1, 333, 4096] {
                let y = run(kind, &fx, &x, block);
                assert!(analysis::max_abs_diff(&reference, &y) < 1e-6, "{kind:?}/{name}: block {block} differs");
            }
        }
    }
}

#[test]
fn every_effect_is_silent_on_silence() {
    let x = signals::silence(RATE, 0.5);
    for kind in EffectKind::ALL {
        // Radio static is the one effect that deliberately makes sound from nothing.
        let fx = if kind == EffectKind::Radio { on(kind, &[("noise", 0.0)]) } else { on(kind, &[]) };
        let y = run(kind, &fx, &x, 480);
        assert!(analysis::peak(&y) < 1e-6, "{kind:?}: {}", analysis::peak(&y));
    }
}

#[test]
fn latency_is_reported_per_enabled_effect() {
    for kind in EffectKind::ALL {
        let params = FxParams::from_settings(&on(kind, &[]));
        let chain = Chain::build(&[kind], &params, RATE as f32, 480);
        let want = match kind {
            EffectKind::Denoise => 960,
            EffectKind::Pitch | EffectKind::Robot => 3,
            _ => 0,
        };
        assert_eq!(chain.latency(), want, "{kind:?}");
    }
}

// ---- specific -------------------------------------------------------------------------------

#[test]
fn gate_closes_on_background_and_opens_on_speech_without_clicks() {
    let mut x = signals::noise(RATE, 0.5, 0.002, 1); // about -57 dBFS hiss
    let speech_start = x.len();
    x.extend(signals::vowel(RATE, 0.5, 150.0));
    x.extend(signals::noise(RATE, 1.0, 0.002, 2));
    let y = run(EffectKind::Gate, &on(EffectKind::Gate, &[("threshold", -45.0), ("reduction", 40.0)]), &x, 480);
    // Background before speech: reduced by ~40 dB once the gate has closed.
    let bg_in = level_db(&x[12_000..speech_start]);
    let bg_out = level_db(&y[12_000..speech_start]);
    assert!(bg_in - bg_out > 35.0, "background only reduced {:.1} dB", bg_in - bg_out);
    // Speech passes at full level.
    let sp = speech_start + 4800..speech_start + 20_000;
    assert!((level_db(&y[sp.clone()]) - level_db(&x[sp])).abs() < 0.5);
    // Tail after hold + release: closed again.
    let tail = y.len() - 12_000..y.len();
    assert!(level_db(&x[tail.clone()]) - level_db(&y[tail]) > 35.0);
    // The 1 ms attack doesn't add steps beyond what the vowel itself has.
    assert!(max_step(&y) <= max_step(&x) * 1.05);
}

#[test]
fn compressor_reduces_loud_and_keeps_quiet() {
    let fx = on(EffectKind::Compressor, &[("threshold", -20.0), ("ratio", 4.0), ("makeup", 0.0)]);
    // -6 dBFS peak sine: 14 dB over, 4:1 -> about 10.5 dB of gain reduction.
    let loud = signals::sine(RATE, 0.5, 300.0, 0.5);
    let y = run(EffectKind::Compressor, &fx, &loud, 480);
    let reduction = 20.0 * (analysis::peak(&loud[12_000..]) / analysis::peak(&y[12_000..])).log10();
    assert!((reduction - 10.5).abs() < 1.0, "reduction {reduction:.2} dB");
    // -40 dBFS: well below threshold and knee, untouched.
    let quiet = signals::sine(RATE, 0.5, 300.0, 0.01);
    let y = run(EffectKind::Compressor, &fx, &quiet, 480);
    assert!((level_db(&y[12_000..]) - level_db(&quiet[12_000..])).abs() < 0.1);
}

#[test]
fn eq_bands_hit_their_gain_and_flat_is_exact() {
    let fx = on(EffectKind::Eq, &[("low", 6.0), ("mid", -6.0), ("high", 4.0)]);
    for (freq, want) in [(40.0, 6.0), (1000.0, -6.0), (16_000.0, 4.0)] {
        let x = signals::sine(RATE, 0.5, freq, 0.25);
        let y = run(EffectKind::Eq, &fx, &x, 480);
        let got = level_db(&y[12_000..]) - level_db(&x[12_000..]);
        assert!((got - want).abs() < 0.8, "{freq} Hz: {got:.2} dB, want {want}");
    }
    let x = signals::vowel(RATE, 0.3, 150.0);
    assert_eq!(run(EffectKind::Eq, &on(EffectKind::Eq, &[]), &x, 480), x);
}

#[test]
fn reverb_tail_length_follows_decay_and_stays_stable() {
    let mut impulse = vec![0.0f32; RATE as usize * 4];
    impulse[0] = 1.0;
    for decay in [0.6f32, 2.0] {
        let fx = on(EffectKind::Reverb, &[("decay", decay), ("predelay", 0.0)]);
        let fxp = FxParams::from_settings(&fx);
        fxp.get(EffectKind::Reverb).slot.mix.store(1.0); // tail only
        let mut chain = Chain::build(&[EffectKind::Reverb], &fxp, RATE as f32, 480);
        let mut y = impulse.clone();
        y.chunks_mut(480).for_each(|c| chain.process(c));
        // Measure the decay slope between 100 ms and the time energy falls 30 dB below that.
        let win = 2400;
        let db: Vec<f32> = y.chunks(win).map(level_db).collect();
        let start = 2; // 100 ms
        let t30 = db[start..].iter().position(|d| *d < db[start] - 30.0).expect("decays") as f32 * win as f32 / RATE as f32;
        let rt60 = 2.0 * t30;
        assert!((rt60 / decay - 1.0).abs() < 0.35, "decay {decay}: measured RT60 {rt60:.2}");
    }
    // Longest, biggest setting doesn't blow up over 20 s of noise.
    let fx = on(EffectKind::Reverb, &[("size", 100.0), ("decay", 10.0), ("damping", 0.0)]);
    let y = run(EffectKind::Reverb, &fx, &signals::noise(RATE, 20.0, 0.3, 3), 480);
    assert!(analysis::peak(&y) < 2.0 && y.iter().all(|s| s.is_finite()));
}

#[test]
fn robot_is_monotone_at_its_pitch() {
    let x = signals::vowel_glide(RATE, 1.0, 120.0, 220.0);
    let fx = on(EffectKind::Robot, &[("pitch_hz", 140.0), ("ring", 0.0), ("metallic", 0.0)]);
    let y = run(EffectKind::Robot, &fx, &x, 480);
    for start in [12_000, 24_000, 36_000] {
        let f = analysis::estimate_f0(&y[start..start + 4800], RATE, 50.0, 800.0).unwrap();
        assert!((f - 140.0).abs() < 4.0, "at {start}: {f}");
    }
}

#[test]
fn radio_is_band_limited() {
    let fx = on(EffectKind::Radio, &[("low_cut", 300.0), ("high_cut", 3400.0), ("drive", 0.0)]);
    for (freq, pass) in [(60.0, false), (1000.0, true), (10_000.0, false)] {
        let x = signals::sine(RATE, 0.4, freq, 0.25);
        let y = run(EffectKind::Radio, &fx, &x, 480);
        let gain = level_db(&y[9600..]) - level_db(&x[9600..]);
        if pass {
            assert!(gain.abs() < 1.0, "{freq} Hz: {gain:.1} dB");
        } else {
            assert!(gain < -30.0, "{freq} Hz only {gain:.1} dB");
        }
    }
}

#[test]
fn denoise_removes_noise_and_keeps_voice() {
    // RNNoise: fan-like noise alone should drop a lot; a voiced vowel should mostly survive.
    // (Flat white hiss is a known RNNoise weak spot; real background noise is coloured.)
    let noise = signals::brown_noise(RATE, 1.5, 0.2, 21);
    let y = run(EffectKind::Denoise, &on(EffectKind::Denoise, &[]), &noise, 480);
    let reduction = level_db(&noise[24_000..]) - level_db(&y[24_000..]);
    assert!(reduction > 12.0, "noise only reduced {reduction:.1} dB");

    let v = signals::vowel(RATE, 1.5, 140.0);
    let y = run(EffectKind::Denoise, &on(EffectKind::Denoise, &[]), &v, 480);
    let loss = level_db(&v[24_000..60_000]) - level_db(&y[24_000..60_000]);
    assert!(loss < 6.0, "vowel lost {loss:.1} dB");
}

#[test]
fn denoise_passes_through_at_unsupported_rates() {
    let fx = FxParams::from_settings(&on(EffectKind::Denoise, &[]));
    let mut chain = Chain::build(&[EffectKind::Denoise], &fx, 44_100.0, 480);
    let x = signals::vowel(44_100, 0.2, 150.0);
    let mut y = x.clone();
    y.chunks_mut(480).for_each(|c| chain.process(c));
    assert_eq!(y, x);
    assert_eq!(chain.latency(), 0);
    assert_eq!(fx.get(EffectKind::Denoise).status.load(std::sync::atomic::Ordering::Relaxed), 1);
}
