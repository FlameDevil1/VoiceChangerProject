//! DSP building blocks. Everything in here must be real-time safe once constructed:
//! no allocation, locking or I/O inside `process` methods.

pub mod drift;
pub mod util;

pub use drift::DriftResampler;
pub use util::{db_to_gain, gain_to_db, SmoothedValue};

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
    /// Clear internal state (delay lines, envelopes) without reallocating.
    fn reset(&mut self) {}
}

/// Processing applied to every block between capture and the outputs.
///
/// This is shared by the real-time engine and (in a later step) the offline renderer, so
/// files and live audio sound identical.
pub struct EngineCore {
    input_gain: SmoothedValue,
    output_gain: SmoothedValue,
    /// 1.0 = effects fully applied, 0.0 = bypassed. Crossfaded to avoid clicks.
    wet: SmoothedValue,
    chain: Vec<Box<dyn Processor>>,
    dry: Vec<f32>,
}

/// Per-block control values, read from atomics by the caller.
#[derive(Clone, Copy, Debug)]
pub struct CoreParams {
    pub input_gain: f32,
    pub output_gain: f32,
    pub bypass: bool,
    pub mute: bool,
}

impl EngineCore {
    pub fn new(sample_rate: f32, max_block: usize) -> Self {
        // 20 ms smoothing: fast enough to feel instant, slow enough to avoid zipper noise.
        let ramp = 0.020;
        Self {
            input_gain: SmoothedValue::new(1.0, sample_rate, ramp),
            output_gain: SmoothedValue::new(1.0, sample_rate, ramp),
            wet: SmoothedValue::new(1.0, sample_rate, ramp),
            chain: Vec::new(),
            dry: vec![0.0; max_block],
        }
    }

    /// Process one block in place. `buf.len()` must not exceed `max_block`.
    pub fn process(&mut self, buf: &mut [f32], p: CoreParams) {
        self.input_gain.set_target(p.input_gain);
        self.output_gain.set_target(if p.mute { 0.0 } else { p.output_gain });
        self.wet.set_target(if p.bypass { 0.0 } else { 1.0 });

        self.input_gain.apply(buf);

        // Skip the chain entirely when it is fully bypassed (or empty) so bypass costs no CPU.
        if !self.chain.is_empty() && !(self.wet.is_settled() && self.wet.current() == 0.0) {
            let dry = &mut self.dry[..buf.len()];
            dry.copy_from_slice(buf);
            for fx in &mut self.chain {
                fx.process(buf);
            }
            // TODO(step 3): delay `dry` by the chain latency before mixing.
            for (w, d) in buf.iter_mut().zip(dry.iter()) {
                let mix = self.wet.next_value();
                *w = *d + (*w - *d) * mix;
            }
        } else {
            self.wet.skip(buf.len());
        }

        self.output_gain.apply(buf);

        // Safety clamp until the limiter (step 3) exists: never send > 0 dBFS to the cable.
        for s in buf.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
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
    fn unity_passthrough_is_bit_exact() {
        let mut core = EngineCore::new(48_000.0, 512);
        let input: Vec<f32> = (0..512).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut buf = input.clone();
        core.process(&mut buf, params());
        assert_eq!(buf, input);
    }

    #[test]
    fn mute_ramps_to_silence_without_jump() {
        let mut core = EngineCore::new(48_000.0, 4800);
        let mut buf = vec![0.5f32; 4800];
        core.process(&mut buf, CoreParams { mute: true, ..params() });
        // First sample barely attenuated (no click), last sample silent after 20 ms ramp.
        assert!(buf[0] > 0.49);
        assert!(buf[4799].abs() < 1e-6);
        let max_step = buf.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.001, "step {max_step}");
    }

    #[test]
    fn output_is_clamped() {
        let mut core = EngineCore::new(48_000.0, 16);
        let mut buf = vec![0.9f32; 16];
        core.process(&mut buf, CoreParams { input_gain: 4.0, ..params() });
        // Gain ramps from 1.0, so just check the clamp holds.
        assert!(buf.iter().all(|s| *s <= 1.0));
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
