//! Headless engine check against real devices.
//!
//!   cargo run --release --example smoke                   list devices
//!   cargo run --release --example smoke -- 10             default mic -> virtual cable for 10 s
//!   cargo run --release --example smoke -- 10 Speakers    same, into a real output with the voice muted
//!
//! Nothing is recorded, and nothing audible is played (monitoring stays off).
//! Set VC_SMOKE_PITCH=1 to run with pitch -5 / formant -3 enabled, VC_SMOKE_ALL=1 for every effect.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;
use voice_changer::audio::{devices, Command, EngineHandle, EngineSettings, Shared};
use voice_changer::dsp::{EffectKind, FxSettings};

fn main() {
    let list = devices::enumerate(&cpal::default_host());
    println!("Inputs (default: {:?}):", list.default_input);
    for d in &list.inputs {
        println!("  {}{}", d.name, if devices::is_cable_capture(&d.name) { "  [cable capture]" } else { "" });
    }
    println!("Outputs (default: {:?}):", list.default_output);
    for d in &list.outputs {
        println!("  {}{}", d.name, if devices::is_cable_playback(&d.name) { "  [virtual cable]" } else { "" });
    }

    let mut args = std::env::args().skip(1);
    let Some(seconds) = args.next().and_then(|s| s.parse::<u64>().ok()) else { return };
    // Optional: substring of a regular output to test with instead of the cable (voice muted).
    let test_out = args.next();
    let cable = match &test_out {
        Some(sub) => list.outputs.iter().find(|d| d.name.contains(sub.as_str())).map(|d| d.to_ref()),
        None => list.cables().next().map(|d| d.to_ref()),
    };
    println!("\nRunning {seconds}s: default mic -> {:?}", cable.as_ref().map(|c| &c.name));

    let shared = Arc::new(Shared::default());
    shared.mute.store(test_out.is_some(), Relaxed);
    if std::env::var_os("VC_SMOKE_PITCH").is_some() {
        let fx = FxSettings::default().with(EffectKind::Pitch, &[("semitones", -5.0), ("formant", -3.0)]);
        shared.fx.store(&fx);
    }
    if std::env::var_os("VC_SMOKE_ALL").is_some() {
        let mut fx = FxSettings::default().with(EffectKind::Pitch, &[("semitones", -5.0), ("formant", -3.0)]);
        for kind in EffectKind::ALL {
            fx.set_enabled(kind, true);
        }
        shared.fx.store(&fx);
    }
    let engine = EngineHandle::spawn(shared.clone(), Box::new(|| {}));
    let chain_order = EffectKind::ALL.to_vec();
    engine.send(Command::Start(EngineSettings { cable, chain_order, ..Default::default() }));
    for _ in 0..seconds {
        std::thread::sleep(Duration::from_secs(1));
        let st = engine.status();
        println!(
            "{:?} | block {} | dsp {} smp | in {:6.1} dB | load {:4.1}% | out fill {:5.1}/{:4.1} ms margin {:2.0} ms drift {:+5.0} ppm underruns {} | xruns {}",
            st.state,
            shared.in_block.load(Relaxed),
            shared.dsp_latency.load(Relaxed),
            20.0 * shared.in_peak.take().max(1e-6).log10(),
            shared.load.take() * 100.0,
            shared.cable.fill_ms.load(),
            shared.cable.target_ms.load(),
            shared.cable.margin_ms.load(),
            shared.cable.correction_ppm.load(),
            shared.cable.underruns.load(Relaxed),
            shared.capture_xruns.load(Relaxed),
        );
    }
    let st = engine.status();
    println!("input: {} @ {} Hz\noutput: {:?}\nwarning: {:?}", st.input_name, st.sample_rate, st.cable, st.warning);
}
