//! Golden-file regression tests for the processing chain.
//!
//! Each case renders a generated signal through `EngineCore` and compares it with
//! `tests/golden/<case>.wav`.
//!
//! - Missing golden: it is written and the test passes (first run of a new case).
//! - Mismatch: the test fails and writes `<case>.actual.wav` next to the golden so you can
//!   listen to both. If the change is intended, re-bless with
//!   `VC_UPDATE_GOLDEN=1 cargo test --test golden`.

use std::path::PathBuf;
use voice_changer::dsp::{CoreParams, EffectKind, FxSettings};
use voice_changer::offline::{self, WavFormat, analysis, signals};

const RATE: u32 = 48_000;
/// Allows for floating-point differences between compilers/CPU targets, nothing audible.
const TOLERANCE: f32 = 1e-5;

struct Case {
    name: &'static str,
    input: fn() -> Vec<f32>,
    params: CoreParams,
    fx: FxSettings,
}

fn pitch(semitones: f32, formant: f32) -> FxSettings {
    fx(EffectKind::Pitch, &[("semitones", semitones), ("formant", formant)])
}

fn fx(kind: EffectKind, values: &[(&str, f32)]) -> FxSettings {
    FxSettings::default().with(kind, values)
}

/// Vowel, a pause with quiet noise, then a quieter vowel: exercises gates, compressors and tails.
fn phrase() -> Vec<f32> {
    let mut x = signals::vowel(RATE, 0.35, 150.0);
    x.extend(signals::noise(RATE, 0.25, 0.003, 7));
    x.extend(signals::vowel(RATE, 0.25, 190.0).iter().map(|s| s * 0.3));
    x
}

fn noisy_vowel() -> Vec<f32> {
    let v = signals::vowel(RATE, 0.6, 140.0);
    let n = signals::brown_noise(RATE, 0.6, 0.1, 11);
    v.iter().zip(&n).map(|(a, b)| a + b).collect()
}

fn cases() -> Vec<Case> {
    let unity = CoreParams::default();
    let none = FxSettings::default;
    vec![
        Case { name: "passthrough_vowel", input: || signals::vowel(RATE, 0.5, 140.0), params: unity, fx: none() },
        Case {
            name: "gain_sweep",
            input: || signals::sweep(RATE, 0.5, 40.0, 16_000.0),
            params: CoreParams { input_gain: 0.5, output_gain: 1.5, ..unity },
            fx: none(),
        },
        Case {
            name: "bypass_noise",
            input: || signals::noise(RATE, 0.25, 0.3, 1),
            params: CoreParams { bypass: true, ..unity },
            fx: pitch(7.0, 0.0),
        },
        Case {
            name: "mute_vowel",
            input: || signals::vowel(RATE, 0.25, 200.0),
            params: CoreParams { mute: true, ..unity },
            fx: none(),
        },
        Case {
            name: "limiter_hot_sweep",
            input: || signals::sweep(RATE, 0.5, 60.0, 8_000.0),
            params: CoreParams { input_gain: 4.0, ..unity },
            fx: none(),
        },
        Case {
            name: "pitch_up7_vowel",
            input: || signals::vowel(RATE, 0.6, 140.0),
            params: unity,
            fx: pitch(7.0, 0.0),
        },
        Case {
            name: "pitch_down12_vowel",
            input: || signals::vowel(RATE, 0.6, 180.0),
            params: unity,
            fx: pitch(-12.0, 0.0),
        },
        Case {
            name: "formant_up4_vowel",
            input: || signals::vowel(RATE, 0.6, 140.0),
            params: unity,
            fx: pitch(0.0, 4.0),
        },
        Case {
            name: "deep_voice_vowel",
            input: || signals::vowel(RATE, 0.6, 160.0),
            params: unity,
            fx: pitch(-5.0, -3.0),
        },
        Case { name: "pitch_noise", input: || signals::noise(RATE, 0.3, 0.3, 2), params: unity, fx: pitch(5.0, 2.0) },
        Case { name: "gate_phrase", input: phrase, params: unity, fx: fx(EffectKind::Gate, &[]) },
        Case { name: "compressor_phrase", input: phrase, params: unity, fx: fx(EffectKind::Compressor, &[]) },
        Case {
            name: "eq_sweep",
            input: || signals::sweep(RATE, 0.5, 40.0, 16_000.0),
            params: unity,
            fx: fx(EffectKind::Eq, &[("low", 6.0), ("mid", -6.0), ("high", 4.0)]),
        },
        Case {
            name: "reverb_hall_phrase",
            input: phrase,
            params: unity,
            fx: fx(EffectKind::Reverb, &[("size", 75.0), ("decay", 2.8)]),
        },
        Case {
            name: "robot_vowel",
            input: || signals::vowel_glide(RATE, 0.6, 120.0, 220.0),
            params: unity,
            fx: fx(EffectKind::Robot, &[]),
        },
        Case { name: "radio_phrase", input: phrase, params: unity, fx: fx(EffectKind::Radio, &[("noise", 30.0)]) },
        Case { name: "denoise_noisy_vowel", input: noisy_vowel, params: unity, fx: fx(EffectKind::Denoise, &[]) },
    ]
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

#[test]
fn golden_outputs_match() {
    let update = std::env::var_os("VC_UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut failures = Vec::new();

    for case in cases() {
        let out = offline::render(&(case.input)(), RATE, case.params, &case.fx, offline::DEFAULT_BLOCK);
        let path = dir.join(format!("{}.wav", case.name));
        let actual_path = dir.join(format!("{}.actual.wav", case.name));
        let _ = std::fs::remove_file(&actual_path);

        if update || !path.exists() {
            offline::save_wav(&path, &out, RATE, WavFormat::Float32).unwrap();
            eprintln!("wrote golden {}", path.display());
            continue;
        }
        let golden = offline::load(&path).unwrap();
        let diff = analysis::max_abs_diff(&golden.samples, &out);
        if golden.rate != RATE || diff > TOLERANCE {
            offline::save_wav(&actual_path, &out, RATE, WavFormat::Float32).unwrap();
            failures.push(format!(
                "{}: max diff {diff:.2e}, SNR {:.1} dB (listen: {})",
                case.name,
                analysis::snr_db(&golden.samples, &out),
                actual_path.display()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "golden mismatches:\n  {}\nIf intended: VC_UPDATE_GOLDEN=1 cargo test --test golden",
        failures.join("\n  ")
    );
}

/// Bypass must be a true null even with effects configured: the output is the input delayed only
/// by the limiter lookahead, bit for bit.
#[test]
fn bypass_is_null() {
    let x = signals::vowel(RATE, 0.3, 160.0);
    let bypass = CoreParams { bypass: true, ..Default::default() };
    let y = offline::render(&x, RATE, bypass, &pitch(7.0, 3.0), offline::DEFAULT_BLOCK);
    let d = voice_changer::dsp::Limiter::new(RATE as f32, voice_changer::dsp::LIMITER_CEILING_DB).latency();
    assert_eq!(analysis::max_abs_diff(&x[..x.len() - d], &y[d..]), 0.0);
}

/// Throughput check, run with `cargo test --release --test golden -- --ignored --nocapture`.
#[test]
#[ignore]
fn render_speed() {
    let x = signals::vowel(RATE, 60.0, 150.0);
    let mut all = pitch(-5.0, -3.0);
    for kind in EffectKind::ALL {
        all.set_enabled(kind, true);
    }
    let mut cases = vec![("no effects".to_string(), FxSettings::default())];
    for kind in EffectKind::ALL {
        cases.push((kind.label().to_string(), fx(kind, &[])));
    }
    cases.push(("all effects".to_string(), all));
    for (label, fx) in cases {
        let t = std::time::Instant::now();
        let y = offline::render(&x, RATE, CoreParams::default(), &fx, offline::DEFAULT_BLOCK);
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(y.len(), x.len());
        println!(
            "{label:>20}: 60 s in {:6.1} ms = {:6.0}x real time = {:.3}% of one core live",
            secs * 1000.0,
            60.0 / secs,
            secs / 60.0 * 100.0
        );
    }
}
