//! Offline processing: run the same `EngineCore` as the live engine over audio files.
//!
//! One code path serves file export, the GUI "Test" button and the DSP test harness, so a
//! file rendered here sounds exactly like the live virtual mic.

pub mod analysis;
pub mod io;
pub mod signals;

use crate::dsp::{Chain, CoreParams, EngineCore, FxParams, FxSettings};

pub use io::{load, save_wav, Audio, WavFormat};

/// Block size used by the live engine at 48 kHz (10 ms WASAPI period). Rendering with the same
/// size by default makes offline output match live output exactly.
pub const DEFAULT_BLOCK: usize = 480;

/// Render mono `input` at `sample_rate` through a fresh `EngineCore` with the effects in `fx`,
/// `block` frames at a time.
///
/// Every effect must produce the same output for any block size; the test suite checks this
/// for each processor, which catches state-handling bugs at block boundaries.
pub fn render(input: &[f32], sample_rate: u32, params: CoreParams, fx: &FxSettings, block: usize) -> Vec<f32> {
    let block = block.clamp(1, crate::dsp::MAX_BLOCK);
    let fx_params = FxParams::from_settings(fx);
    let chain = Chain::build(&fx.order, &fx_params, sample_rate as f32, block);
    let mut core = EngineCore::with_chain(sample_rate as f32, block, chain);
    let mut out = input.to_vec();
    for chunk in out.chunks_mut(block) {
        core.process(chunk, params);
    }
    out
}

/// Downmix interleaved audio to mono. `channel = None` averages all channels.
pub fn to_mono(audio: &Audio, channel: Option<usize>) -> Vec<f32> {
    let mut mono = vec![0.0; audio.frames()];
    crate::dsp::downmix(&audio.samples, audio.channels, channel, &mut mono, |s| s);
    mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_block_size_invariant() {
        let x = signals::vowel(48_000, 0.5, 140.0);
        let params = CoreParams { input_gain: 0.7, output_gain: 1.2, ..Default::default() };
        let mut fx = FxSettings::default();
        fx.pitch.enabled = true;
        fx.pitch.semitones = -3.0;
        fx.pitch.formant = 2.0;
        let reference = render(&x, 48_000, params, &fx, DEFAULT_BLOCK);
        for block in [1, 64, 333, 4096] {
            let y = render(&x, 48_000, params, &fx, block);
            assert!(analysis::max_abs_diff(&reference, &y) < 1e-6, "block {block}");
        }
    }

    #[test]
    fn to_mono_averages() {
        let a = Audio { rate: 48_000, channels: 2, samples: vec![1.0, 0.0, 0.5, 0.5] };
        assert_eq!(to_mono(&a, None), vec![0.5, 0.5]);
        assert_eq!(to_mono(&a, Some(1)), vec![0.0, 0.5]);
    }
}
