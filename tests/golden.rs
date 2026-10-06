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
use voice_changer::dsp::CoreParams;
use voice_changer::offline::{self, analysis, signals, WavFormat};

const RATE: u32 = 48_000;
/// Allows for floating-point differences between compilers/CPU targets, nothing audible.
const TOLERANCE: f32 = 1e-5;

struct Case {
    name: &'static str,
    input: fn() -> Vec<f32>,
    params: CoreParams,
}

fn cases() -> Vec<Case> {
    let unity = CoreParams::default();
    vec![
        Case { name: "passthrough_vowel", input: || signals::vowel(RATE, 0.5, 140.0), params: unity },
        Case {
            name: "gain_sweep",
            input: || signals::sweep(RATE, 0.5, 40.0, 16_000.0),
            params: CoreParams { input_gain: 0.5, output_gain: 1.5, ..unity },
        },
        Case { name: "bypass_noise", input: || signals::noise(RATE, 0.25, 0.3, 1), params: CoreParams { bypass: true, ..unity } },
        Case { name: "mute_vowel", input: || signals::vowel(RATE, 0.25, 200.0), params: CoreParams { mute: true, ..unity } },
        // Effects added in later steps get their cases here (pitch_up_vowel, robot_vowel, ...).
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
        let out = offline::render(&(case.input)(), RATE, case.params, offline::DEFAULT_BLOCK);
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

/// Bypass must be a true null: output identical to input (once effects with latency exist, this
/// also checks the dry path is delay-compensated).
#[test]
fn bypass_is_null() {
    let x = signals::vowel(RATE, 0.3, 160.0);
    let y = offline::render(&x, RATE, CoreParams { bypass: true, ..Default::default() }, offline::DEFAULT_BLOCK);
    assert_eq!(analysis::max_abs_diff(&x, &y), 0.0);
}

/// Throughput check, run with `cargo test --release --test golden -- --ignored --nocapture`.
#[test]
#[ignore]
fn render_speed() {
    let x = signals::vowel(RATE, 60.0, 150.0);
    let t = std::time::Instant::now();
    let y = offline::render(&x, RATE, CoreParams::default(), offline::DEFAULT_BLOCK);
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(y.len(), x.len());
    println!("60 s rendered in {:.1} ms ({:.0}x real time)", secs * 1000.0, 60.0 / secs);
}
