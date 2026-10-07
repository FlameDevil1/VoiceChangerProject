//! DSP building blocks. Everything in here must be real-time safe once constructed:
//! no allocation, locking or I/O inside `process` methods.

pub mod biquad;
pub mod chain;
pub mod drift;
pub mod fx;
pub mod limiter;
pub mod params;
pub mod pitch;
pub mod shared_params;
pub mod simd;
pub mod util;

pub use chain::{Chain, EffectKind, FxParams, FxSettings};
pub use drift::DriftResampler;
pub use limiter::Limiter;
pub use util::{SmoothedValue, db_to_gain, gain_to_db};

/// Output ceiling of the always-on limiter.
pub const LIMITER_CEILING_DB: f32 = -1.0;

/// Largest block processed in one call; bigger callbacks are split. Scratch buffers use this size.
pub const MAX_BLOCK: usize = 4096;

/// A mono audio processor in the effect chain.
///
/// Effects added in later build steps (pitch, reverb, ...) implement this. The engine calls
/// `prepare` off the audio thread, then `process` from the audio callback.
pub trait Processor: Send {
    /// Allocate buffers for `sample_rate` and blocks of at most `max_block` frames.
    fn prepare(&mut self, sample_rate: f32, max_block: usize);
    /// Process `buf` in place.
    fn process(&mut self, buf: &mut [f32]);
    /// Algorithmic latency in samples. The chain delays the dry signal by this much so
    /// wet/dry mixing does not comb-filter.
    fn latency(&self) -> usize {
        0
    }
    /// Largest latency this effect can ever report (sizes the slot's dry-path delay line).
    fn max_latency(&self) -> usize {
        self.latency()
    }
    /// Clear internal state (delay lines, envelopes) without reallocating.
    fn reset(&mut self) {}
}

/// Processing applied to every block between capture and the outputs:
/// input gain -> effect chain (bypassable) -> output gain / mute -> limiter.
///
/// Shared by the real-time engine and the offline renderer, so files and live audio sound
/// identical.
pub struct EngineCore {
    input_gain: SmoothedValue,
    output_gain: SmoothedValue,
    /// 1.0 = effects fully applied, 0.0 = bypassed. Crossfaded to avoid clicks.
    wet: SmoothedValue,
    chain: Chain,
    /// Previous chain, still playing while a new one fades in.
    fading_out: Option<Chain>,
    swap_fade: SmoothedValue,
    /// Chain that finished fading out; the owner must dispose of it off the audio thread.
    retired: Option<Chain>,
    sample_rate: f32,
    /// Parameters start at their first values instead of ramping from defaults.
    first_block: bool,
    limiter: Limiter,
    dry: Vec<f32>,
    old_out: Vec<f32>,
}

/// Per-block control values, read from atomics by the caller.
#[derive(Clone, Copy, Debug)]
pub struct CoreParams {
    pub input_gain: f32,
    pub output_gain: f32,
    pub bypass: bool,
    pub mute: bool,
}

impl Default for CoreParams {
    /// Unity gain, effects on, not muted.
    fn default() -> Self {
        Self { input_gain: 1.0, output_gain: 1.0, bypass: false, mute: false }
    }
}

impl EngineCore {
    pub fn new(sample_rate: f32, max_block: usize) -> Self {
        Self::with_chain(sample_rate, max_block, Chain::empty())
    }

    pub fn with_chain(sample_rate: f32, max_block: usize, chain: Chain) -> Self {
        // 20 ms smoothing: fast enough to feel instant, slow enough to avoid zipper noise.
        let ramp = 0.020;
        Self {
            input_gain: SmoothedValue::new(1.0, sample_rate, ramp),
            output_gain: SmoothedValue::new(1.0, sample_rate, ramp),
            wet: SmoothedValue::new(1.0, sample_rate, ramp),
            chain,
            fading_out: None,
            swap_fade: SmoothedValue::new(1.0, sample_rate, ramp),
            retired: None,
            sample_rate,
            first_block: true,
            limiter: Limiter::new(sample_rate, LIMITER_CEILING_DB),
            dry: vec![0.0; max_block],
            old_out: vec![0.0; max_block],
        }
    }

    /// Replace the effect chain with a 20 ms crossfade (e.g. reordering, loading a preset).
    ///
    /// Returns a chain that must be disposed of immediately (only when a previous swap was still
    /// fading). Chains that finish fading are available from `take_retired`. Neither is dropped
    /// here, so no deallocation ever happens on the audio thread.
    #[must_use]
    pub fn set_chain(&mut self, new: Chain) -> Option<Chain> {
        let old = std::mem::replace(&mut self.chain, new);
        let dispose = self.fading_out.replace(old);
        self.swap_fade = SmoothedValue::new(0.0, self.sample_rate, 0.020);
        self.swap_fade.set_target(1.0);
        dispose
    }

    pub fn take_retired(&mut self) -> Option<Chain> {
        self.retired.take()
    }

    /// Total delay through the core in samples (active effects + limiter lookahead).
    pub fn latency(&self) -> usize {
        self.chain.latency() + self.limiter.latency()
    }

    /// Process one block in place. `buf.len()` must not exceed `max_block`.
    pub fn process(&mut self, buf: &mut [f32], p: CoreParams) {
        util::enable_ftz();
        self.input_gain.set_target(p.input_gain);
        self.output_gain.set_target(if p.mute { 0.0 } else { p.output_gain });
        self.wet.set_target(if p.bypass { 0.0 } else { 1.0 });
        if std::mem::take(&mut self.first_block) {
            self.input_gain.snap();
            self.output_gain.snap();
            self.wet.snap();
        }

        self.input_gain.apply(buf);

        // Skip the chain entirely when it is fully bypassed (or empty) so bypass costs no CPU.
        let bypassed = self.wet.is_settled() && self.wet.current() == 0.0;
        if (!self.chain.is_empty() || self.fading_out.is_some()) && !bypassed {
            let n = buf.len();
            self.dry[..n].copy_from_slice(buf);
            if let Some(old) = &mut self.fading_out {
                let old_out = &mut self.old_out[..n];
                old_out.copy_from_slice(buf);
                old.process(old_out);
                self.chain.process(buf);
                for (y, o) in buf.iter_mut().zip(old_out.iter()) {
                    let f = self.swap_fade.next_value();
                    *y = *o + (*y - *o) * f;
                }
                if self.swap_fade.is_settled() {
                    self.retired = self.fading_out.take();
                }
            } else {
                self.chain.process(buf);
            }
            // Bypass toggles crossfade against the undelayed input: "bypass" means hearing your
            // own voice with the least delay, and the 20 ms fade hides the brief mismatch.
            if !(self.wet.is_settled() && self.wet.current() == 1.0) {
                for (w, d) in buf.iter_mut().zip(self.dry[..n].iter()) {
                    let mix = self.wet.next_value();
                    *w = *d + (*w - *d) * mix;
                }
            }
        } else {
            self.wet.skip(buf.len());
        }

        self.output_gain.apply(buf);
        self.limiter.process(buf);
    }
}

/// Downmix interleaved frames into mono. `channel = None` averages all channels,
/// `Some(c)` picks one (for interfaces with the mic on a single channel).
pub fn downmix<T: Copy>(
    interleaved: &[T],
    channels: usize,
    channel: Option<usize>,
    out: &mut [f32],
    to_f32: impl Fn(T) -> f32,
) -> usize {
    let frames = (interleaved.len() / channels).min(out.len());
    match channel {
        Some(c) if c < channels => {
            for (o, frame) in out.iter_mut().zip(interleaved.chunks_exact(channels)) {
                *o = to_f32(frame[c]);
            }
        }
        _ if channels == 1 => {
            for (o, s) in out.iter_mut().zip(interleaved.iter()) {
                *o = to_f32(*s);
            }
        }
        _ => {
            let scale = 1.0 / channels as f32;
            for (o, frame) in out.iter_mut().zip(interleaved.chunks_exact(channels)) {
                *o = frame.iter().map(|s| to_f32(*s)).sum::<f32>() * scale;
            }
        }
    }
    frames
}

/// Absolute peak of a block.
pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> CoreParams {
        CoreParams { input_gain: 1.0, output_gain: 1.0, bypass: false, mute: false }
    }

    #[test]
    fn unity_passthrough_is_bit_exact_after_limiter_delay() {
        let mut core = EngineCore::new(48_000.0, 512);
        let input: Vec<f32> = (0..512).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut buf = input.clone();
        core.process(&mut buf, params());
        let d = core.latency();
        assert_eq!(&buf[d..], &input[..512 - d]);
    }

    #[test]
    fn mute_ramps_to_silence_without_jump() {
        let mut core = EngineCore::new(48_000.0, 4800);
        let mut warm = vec![0.5f32; 4800];
        core.process(&mut warm, params());
        let mut buf = vec![0.5f32; 4800];
        core.process(&mut buf, CoreParams { mute: true, ..params() });
        // Muting mid-stream: starts at full level (no click), silent after the 20 ms ramp.
        assert!(buf[0] > 0.49);
        assert!(buf[4799].abs() < 1e-6);
        let max_step = buf.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.001, "step {max_step}");
    }

    #[test]
    fn first_block_starts_at_requested_values() {
        // A file rendered at -6 dB must be at -6 dB from the first sample, not ramp from 0 dB.
        let mut core = EngineCore::new(48_000.0, 4800);
        let mut buf = vec![0.5f32; 4800];
        core.process(&mut buf, CoreParams { input_gain: 0.5, ..params() });
        let d = core.latency();
        assert!(buf[d..].iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn output_never_exceeds_ceiling() {
        let mut core = EngineCore::new(48_000.0, 4800);
        let mut buf: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.05).sin() * 0.9).collect();
        core.process(&mut buf, CoreParams { input_gain: 4.0, ..params() });
        assert!(peak(&buf) <= db_to_gain(LIMITER_CEILING_DB) + 1e-6);
    }

    #[test]
    fn chain_swap_crossfades_and_retires_old_chain() {
        use crate::offline::signals;
        let fx = FxParams::from_settings(&FxSettings::default().with(EffectKind::Pitch, &[("semitones", 7.0)]));
        let mut core = EngineCore::with_chain(48_000.0, 480, Chain::empty());
        let x = signals::vowel(48_000, 0.5, 150.0);
        let mut y = x.clone();
        for (i, c) in y.chunks_mut(480).enumerate() {
            if i == 20 {
                assert!(core.set_chain(Chain::build(&[EffectKind::Pitch], &fx, 48_000.0, 480)).is_none());
            }
            core.process(c, params());
        }
        assert!(core.take_retired().is_some(), "old chain handed back after the fade");
        let max_in = x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        let max_out = y.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_out < max_in * 2.0, "click: in {max_in}, out {max_out}");
    }

    #[test]
    fn downmix_average_and_select() {
        let stereo = [1.0f32, 0.0, 0.5, 0.5];
        let mut out = [0.0f32; 2];
        assert_eq!(downmix(&stereo, 2, None, &mut out, |s| s), 2);
        assert_eq!(out, [0.5, 0.5]);
        downmix(&stereo, 2, Some(0), &mut out, |s| s);
        assert_eq!(out, [1.0, 0.5]);
    }
}
